use super::super::{Source, store::Message};
use super::{sessions, settings, worker::eligible};
use crate::{AppState, error::ApiResult};
use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

// 收到最后一条补充后等三秒；持续输入最多合并十秒，冷却时间另行约束。
const DEBOUNCE_SECONDS: f64 = 3.0;
const MAX_BATCH_SECONDS: f64 = 10.0;

/// 增量采集和发送前复查共用同一个版本化入队入口；调用方持有沟通锁。
pub(crate) async fn enqueue(
    state: &AppState,
    source: &Source,
    connection_version: i64,
    messages: &[Message],
) -> ApiResult<()> {
    if state.config.typesafe.is_none() {
        return Ok(());
    }
    let settings = settings::load(state).await?;
    if !settings.enabled || settings.rules_error.is_some() {
        return Ok(());
    }
    let connection: Option<(Uuid, String)> = sqlx::query_as("SELECT private_discovery_generation,open_id FROM communication_connections WHERE owner='admin' AND version=$1 AND status='active' AND send_authorized")
        .bind(connection_version).fetch_optional(&state.pool).await?;
    let Some((generation, open_id)) = connection else {
        return Ok(());
    };
    let self_test = settings.self_test_chat_id.as_deref() == Some(&source.chat_id);
    let now = Utc::now().timestamp_millis();
    let mut tx = state.pool.begin().await?;
    sqlx::query("INSERT INTO communication_takeover_sessions(source_id,source_version,connection_version,connection_generation,settings_version,epoch,boundary_ms,mode) VALUES($1,$2,$3,$4,$5,$6,$7,CASE WHEN EXISTS(SELECT 1 FROM communication_takeover_jobs WHERE source_id=$1 AND status IN ('dispatching','unknown')) THEN 'uncertain' ELSE 'auto' END) ON CONFLICT DO NOTHING")
        .bind(source.id).bind(source.version).bind(connection_version).bind(generation).bind(settings.version).bind(Uuid::new_v4()).bind(settings.since_ms).execute(&mut *tx).await?;
    let mut session = sessions::lock(&mut tx, source.id).await?;
    let changed: bool = sqlx::query_scalar("SELECT source_version<>$2 OR connection_version<>$3 OR connection_generation<>$4 OR settings_version<>$5 FROM communication_takeover_sessions WHERE source_id=$1")
        .bind(source.id).bind(source.version).bind(connection_version).bind(generation).bind(settings.version).fetch_one(&mut *tx).await?;
    if changed {
        sessions::cancel(&mut tx, source.id, "scope_changed").await?;
        session.epoch = Uuid::new_v4();
        session.boundary_ms = session.boundary_ms.max(settings.since_ms);
        session.last_activity_ms = 0;
        sqlx::query("UPDATE communication_takeover_sessions SET source_version=$2,connection_version=$3,connection_generation=$4,settings_version=$5,epoch=$6,boundary_ms=$7,version=version+1,topic=NULL,last_activity_ms=0,updated_at=now() WHERE source_id=$1")
            .bind(source.id).bind(source.version).bind(connection_version).bind(generation).bind(settings.version).bind(session.epoch).bind(session.boundary_ms).execute(&mut *tx).await?;
    }
    let mut ordered: Vec<&Message> = messages
        .iter()
        .filter(|m| m.chat_id == source.chat_id)
        .collect();
    ordered.sort_by_key(|m| (m.create_time, &m.message_id));
    for message in ordered {
        // 发送 ID 防回采循环；迁移前的原消息也不能因新队列为空而再次接管。
        let agent: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM communication_takeover_jobs WHERE source_id=$1 AND (sent_message_id=$2 OR (turn_id IS NULL AND message_id=$2)))")
            .bind(source.id).bind(&message.message_id).fetch_one(&mut *tx).await?;
        if agent || super::is_agent_message(&message.text) {
            continue;
        }
        let previous: Option<Value> = sqlx::query_scalar("SELECT message FROM communication_takeover_inputs WHERE source_id=$1 AND message_id=$2")
            .bind(source.id).bind(&message.message_id).fetch_optional(&mut *tx).await?;
        if previous.is_none()
            && (message.create_time < session.boundary_ms
                || message.create_time < now - super::MAX_AGE_MS
                || message.create_time > now)
        {
            continue;
        }
        let snapshot = json!(message);
        // 名称或采集附加元数据变化不产生新问题；旧版本重放不能覆盖较新的编辑。
        if let Some(old) = &previous
            && ((old["update_time"].as_i64().unwrap_or(0) > message.update_time
                && !message.deleted)
                || same_input(old, &snapshot))
        {
            continue;
        }
        sqlx::query("INSERT INTO communication_takeover_inputs(source_id,message_id,message) VALUES($1,$2,$3) ON CONFLICT(source_id,message_id) DO UPDATE SET message=excluded.message")
            .bind(source.id).bind(&message.message_id).bind(&snapshot).execute(&mut *tx).await?;
        let mut pending: Option<(Uuid, Value)> = sqlx::query_as("SELECT id,inputs FROM communication_takeover_turns WHERE source_id=$1 AND status='pending'")
            .bind(source.id).fetch_optional(&mut *tx).await?;
        let member = pending.as_ref().is_some_and(|(_, inputs)| {
            inputs
                .as_array()
                .is_some_and(|items| items.iter().any(|m| m["message_id"] == message.message_id))
        });
        if let Some(previous) = &previous
            && !member
        {
            // 已回答或被跳过的输入被编辑时，只隔离旧上下文，不自行重答。
            if !same_input(previous, &snapshot) {
                session.epoch = Uuid::new_v4();
                sqlx::query("UPDATE communication_takeover_sessions SET epoch=$2,topic=NULL,version=version+1 WHERE source_id=$1")
                    .bind(source.id).bind(session.epoch).execute(&mut *tx).await?;
                sessions::cancel(&mut tx, source.id, "history_changed").await?;
            }
            continue;
        }
        if message.create_time < session.boundary_ms
            || message.create_time < now - super::MAX_AGE_MS
            || message.create_time > now
            || message.sender_type != "user"
        {
            continue;
        }
        // 必须先判断上一次截止时间，再记录本条活动；否则每条新消息都会续期，永远无法恢复。
        if session.mode == "human"
            && session
                .human_until_ms
                .is_some_and(|until| message.create_time > until)
        {
            session.mode = "auto".into();
            session.boundary_ms = message.create_time;
            session.epoch = Uuid::new_v4();
            sqlx::query("UPDATE communication_takeover_sessions SET mode='auto',epoch=$2,boundary_ms=$3,human_until_ms=NULL,topic=NULL,version=version+1 WHERE source_id=$1")
                .bind(source.id).bind(session.epoch).bind(session.boundary_ms).execute(&mut *tx).await?;
        } else if session.mode == "auto"
            && session.last_activity_ms > 0
            && message.create_time - session.last_activity_ms > sessions::IDLE_MS
        {
            sessions::cancel(&mut tx, source.id, "episode_expired").await?;
            pending = None;
            session.epoch = Uuid::new_v4();
            sqlx::query("UPDATE communication_takeover_sessions SET epoch=$2,topic=NULL,version=version+1 WHERE source_id=$1")
                .bind(source.id).bind(session.epoch).execute(&mut *tx).await?;
        }
        session.last_activity_ms = session.last_activity_ms.max(message.create_time);
        if message.is_me && !self_test && !message.deleted {
            sessions::cancel(&mut tx, source.id, "owner_replied").await?;
            // 本人发言不能覆盖手动暂停或发送待核对。
            if session.mode == "auto" || session.mode == "human" {
                session.mode = "human".into();
            }
        }
        session.human_until_ms = if session.mode == "human" {
            Some(session.last_activity_ms + sessions::IDLE_MS)
        } else {
            None
        };
        sqlx::query("UPDATE communication_takeover_sessions SET mode=$2,last_activity_ms=$3,human_until_ms=$4,version=version+1,updated_at=now() WHERE source_id=$1")
            .bind(source.id).bind(&session.mode).bind(session.last_activity_ms).bind(session.human_until_ms).execute(&mut *tx).await?;
        if session.mode != "auto" {
            continue;
        }
        if !eligible(
            message,
            session.boundary_ms,
            now,
            self_test && message.sender_id == open_id,
        ) {
            if member {
                sessions::cancel(&mut tx, source.id, "input_recalled").await?;
            }
            continue;
        }
        let (turn, mut inputs) = match pending {
            Some((id, inputs)) => (id, inputs.as_array().cloned().unwrap_or_default()),
            None => (Uuid::new_v4(), vec![]),
        };
        inputs.retain(|m| m["message_id"] != message.message_id);
        inputs.push(snapshot.clone());
        inputs.sort_by_key(|m| {
            (
                m["create_time"].as_i64().unwrap_or(0),
                m["message_id"].as_str().unwrap_or("").to_owned(),
            )
        });
        // 每个版本都保留完整成员，旧生成立即失效；冷却只影响领取时间，不丢掉新问题。
        sqlx::query("UPDATE communication_takeover_jobs SET status='ignored',reason='superseded',updated_at=now() WHERE source_id=$1 AND status IN ('queued','evaluating')")
            .bind(source.id).execute(&mut *tx).await?;
        let revision: i64 = sqlx::query_scalar("INSERT INTO communication_takeover_turns(id,source_id,epoch,inputs,available_at) VALUES($1,$2,$3,$4,now()+make_interval(secs=>$5)) ON CONFLICT(id) DO UPDATE SET inputs=excluded.inputs,revision=communication_takeover_turns.revision+1,available_at=LEAST(now()+make_interval(secs=>$5),communication_takeover_turns.started_at+make_interval(secs=>$6)) RETURNING revision")
            .bind(turn).bind(source.id).bind(session.epoch).bind(json!(inputs)).bind(DEBOUNCE_SECONDS).bind(MAX_BATCH_SECONDS).fetch_one(&mut *tx).await?;
        // 回复锚点固定为这一批的最后一条文字消息，所有前序输入仍进入上下文。
        let anchor = inputs.last().expect("轮次至少有一条消息");
        sqlx::query("INSERT INTO communication_takeover_jobs(id,source_id,message_id,source_version,connection_version,connection_generation,settings_version,message,turn_id,turn_revision) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
            .bind(Uuid::new_v4()).bind(source.id).bind(anchor["message_id"].as_str()).bind(source.version).bind(connection_version).bind(generation).bind(settings.version).bind(anchor).bind(turn).bind(revision).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}
/// 只比较影响回答的原始字段，不让名称解析、同步附加字段触发重复生成。
pub(super) fn same_input(a: &Value, b: &Value) -> bool {
    [
        "message_id",
        "text",
        "deleted",
        "sender_id",
        "sender_type",
        "sender_id_type",
        "is_me",
        "message_type",
        "create_time",
        "update_time",
    ]
    .iter()
    .all(|key| a[*key] == b[*key])
}
