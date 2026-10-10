use super::super::{SOURCE_COLUMNS, Source, client, store::Message, sync};
use super::{AGENT_PREFIX, MAX_AGE_MS, context, decision, evidence, sessions, settings, turns};
use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

// 为远端最新消息检查保留有限预算；看不到原消息时不猜测是否仍可回复。
const RECENT_MESSAGES: &str = "20";
const MAX_INPUT_CHARS: usize = 4000;
const JOB_SECONDS: u64 = 90;

/// 队列绑定三层版本，断开重连、暂停来源或改规则都令旧任务失效。
#[derive(sqlx::FromRow)]
struct Job {
    /// 稳定投递幂等键。
    id: Uuid,
    /// 所属私聊来源。
    source_id: Uuid,
    /// 归一化的原消息快照。
    message: Value,
    /// 来源订阅版本。
    source_version: i64,
    /// 用户连接版本。
    connection_version: i64,
    /// 删除再建连接也不复用的代际。
    connection_generation: Uuid,
    /// 管理员确认的话题和开关版本。
    settings_version: i64,
    /// 当前聚合轮次和不可复用的输入版本。
    turn_id: Uuid,
    /// 生成、复核及投递必须始终命中该版本。
    turn_revision: i64,
}
/// 只处理新鲜真人文本；本人仅在已绑定的自聊测试中放行，机器人和旧消息仍排除。
pub(super) fn eligible(message: &Message, since_ms: i64, now: i64, self_test: bool) -> bool {
    (!message.is_me || self_test)
        && !message.deleted
        && message.sender_type == "user"
        && message.sender_id_type == "open_id"
        && message.sender_id.starts_with("ou_")
        && message.message_type == "text"
        && !message.text.trim().is_empty()
        && message.text.chars().count() <= MAX_INPUT_CHARS
        && !super::is_agent_message(&message.text)
        && message.create_time >= since_ms
        && message.create_time >= now - MAX_AGE_MS
        && message.create_time <= now
        && message.message_id.starts_with("om_")
        && message.message_id.len() <= 128
        && message
            .message_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
}
/// 每步只领取一项；崩溃后的未知发送不自动重试，避免重复代表本人发言。
pub async fn step(state: &AppState) -> ApiResult<()> {
    if state.config.typesafe.is_none() {
        return Ok(());
    }
    sessions::quarantine(state).await?;
    sessions::recover_evaluations(state).await?;
    let settings = settings::load(state).await?;
    if !settings.enabled || settings.rules_error.is_some() {
        return Ok(());
    }
    let job: Option<Job> = sqlx::query_as(include_str!("../../sql/takeover_claim.sql"))
        .fetch_optional(&state.pool)
        .await?;
    let Some(job) = job else { return Ok(()) };
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(JOB_SECONDS),
        process(state, &job),
    )
    .await;
    let outcome = match result {
        Ok(Ok(reason)) => ("ignored", reason),
        Ok(Err(error)) => ("failed", error.1),
        Err(_) => ("failed", "takeover_timeout"),
    };
    // 只有当前版本能结束轮次；被补充消息替代的旧工作不能关闭新轮次。
    let mut tx = state.pool.begin().await?;
    let exists: Option<Uuid> = sqlx::query_scalar(
        "SELECT source_id FROM communication_takeover_sessions WHERE source_id=$1 FOR UPDATE",
    )
    .bind(job.source_id)
    .fetch_optional(&mut *tx)
    .await?;
    if exists.is_some() {
        let status: Option<String> = sqlx::query_scalar("UPDATE communication_takeover_jobs SET status=CASE WHEN status='dispatching' THEN 'unknown' ELSE $2 END,reason=$3,updated_at=now() WHERE id=$1 AND status IN ('evaluating','dispatching') RETURNING status")
            .bind(job.id).bind(outcome.0).bind(outcome.1).fetch_optional(&mut *tx).await?;
        if status.as_deref() == Some("ignored")
            && ["not_matched", "cancelled_by_sender", "conversation_closed"].contains(&outcome.1)
        {
            sqlx::query("UPDATE communication_takeover_sessions SET topic=NULL,epoch=$2,version=version+1 WHERE source_id=$1")
                .bind(job.source_id).bind(Uuid::new_v4()).execute(&mut *tx).await?;
        }
        if status.as_deref() == Some("unknown") {
            sessions::uncertain(&mut tx, job.source_id).await?;
        }
        sqlx::query(
            "UPDATE communication_takeover_turns SET status='closed' WHERE id=$1 AND revision=$2",
        )
        .bind(job.turn_id)
        .bind(job.turn_revision)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
/// 所有判断、生成和证据读取完成后才进入发送阶段。
async fn process(state: &AppState, job: &Job) -> ApiResult<&'static str> {
    let settings = settings::load(state).await?;
    if !active(state, job).await? {
        return Ok("scope_changed");
    }
    let message: Message =
        serde_json::from_value(job.message.clone()).map_err(super::super::unavailable)?;
    if !eligible(
        &message,
        settings.since_ms,
        Utc::now().timestamp_millis(),
        settings.self_test_chat_id.as_deref() == Some(message.chat_id.as_str()),
    ) {
        return Ok("expired");
    }
    // 保留本次实际使用的阈值，后续修改设置不能改变旧记录的解释。
    sqlx::query("UPDATE communication_takeover_jobs SET decision_threshold=$2 WHERE id=$1 AND status='evaluating'")
        .bind(job.id).bind(settings.threshold).execute(&state.pool).await?;
    let topics: Vec<String> =
        serde_json::from_value(settings.topics).map_err(super::super::unavailable)?;
    let Some(context) = context::snapshot(state, job.turn_id, job.turn_revision).await? else {
        return Ok("context_unavailable");
    };
    if let Some(reason) = context::closure(&context.question) {
        return Ok(reason);
    }
    let mut input = context.input;
    let Some((topic, probability)) =
        decision::matching(state, input.clone(), &topics, settings.threshold).await?
    else {
        return Ok("not_matched");
    };
    let mut clarified = context.clarified;
    // 换话题后丢弃旧话题正文；匹配阶段可用旧上下文判断指代，起草只接收当前话题。
    if input["previous_topic"]
        .as_str()
        .is_some_and(|previous| previous != topic)
    {
        input["history"] = json!([]);
        input["clarification_allowed"] = json!(true);
        clarified = false;
    }
    sqlx::query("UPDATE communication_takeover_jobs SET topic=$2,probability=$3 WHERE id=$1 AND status='evaluating'").bind(job.id).bind(&topic).bind(probability).execute(&state.pool).await?;
    let evidence = evidence::gather(state, &context.question, &topic).await?;
    input["allowed_topic"] = json!(topic);
    input["evidence"] = json!(evidence);
    // 仅保存对话快照，不重复保存可被撤回的知识正文。
    let mut saved_context = input.clone();
    saved_context
        .as_object_mut()
        .expect("上下文对象")
        .remove("evidence");
    sqlx::query(
        "UPDATE communication_takeover_jobs SET context=$2 WHERE id=$1 AND status='evaluating'",
    )
    .bind(job.id)
    .bind(&saved_context)
    .execute(&state.pool)
    .await?;
    let generated = state
        .runtime
        .takeover_answer(&input)
        .await
        .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "takeover_answer_unavailable"))?;
    let kind = generated["kind"].as_str().unwrap_or("answer").to_owned();
    let answer = if kind == "clarify" {
        if clarified {
            return Ok("clarification_limit");
        }
        evidence::clarification(generated)
    } else if kind == "answer" {
        evidence::validate(generated, &evidence)
    } else {
        None
    };
    let Some(answer) = answer else {
        return Ok(if evidence.is_empty() {
            "no_evidence"
        } else {
            "unanswerable"
        });
    };
    input["reply_kind"] = json!(kind);
    sqlx::query(
        "UPDATE communication_takeover_jobs SET reply_kind=$2 WHERE id=$1 AND status='evaluating'",
    )
    .bind(job.id)
    .bind(&kind)
    .execute(&state.pool)
    .await?;
    // 草稿在复核前单独保存；即使低分或复核故障，也不冒充已进入投递阶段的 answer。
    let saved = sqlx::query("UPDATE communication_takeover_jobs SET draft_answer=$2 WHERE id=$1 AND status='evaluating'")
        .bind(job.id).bind(&answer).execute(&state.pool).await?;
    if saved.rows_affected() != 1 {
        return Ok("scope_changed");
    }
    input["proposed_answer"] = json!(answer);
    let review_probability = decision::review(state, input).await?;
    let saved = sqlx::query("UPDATE communication_takeover_jobs SET review_probability=$2 WHERE id=$1 AND status='evaluating'")
        .bind(job.id).bind(review_probability).execute(&state.pool).await?;
    if saved.rows_affected() != 1 {
        return Ok("scope_changed");
    }
    if review_probability < settings.threshold {
        return Ok("answer_not_supported");
    }
    let token = client::access(state).await?;
    let open_id: String =
        sqlx::query_scalar("SELECT open_id FROM communication_connections WHERE owner='admin'")
            .fetch_one(&state.pool)
            .await?;
    if !fresh_remote(state, job, &message, &open_id, &token).await? {
        return Ok("conversation_changed");
    }
    // 文件检查在锁设置行之前完成，避免在另一个连接上重复取得同一行锁。
    if settings::load(state).await?.version != job.settings_version {
        return Ok("scope_changed");
    }
    // 发送期间与关闭、遗忘、资料修正串行；网络请求受 client 的五秒上限约束。
    let _communication = state.communications.lock().await;
    let mut send_tx = state.pool.begin().await?;
    let exists: Option<Uuid> = sqlx::query_scalar(
        "SELECT source_id FROM communication_takeover_sessions WHERE source_id=$1 FOR UPDATE",
    )
    .bind(job.source_id)
    .fetch_optional(&mut *send_tx)
    .await?;
    if exists.is_none() {
        return Ok("scope_changed");
    }
    // 发送的有限网络窗口内也锁住授权、订阅及设置行，覆盖多个服务进程的修改。
    sqlx::query("SELECT s.id FROM communication_sources s JOIN communication_connections c ON c.owner=s.owner JOIN communication_takeover_settings t ON t.owner=c.owner WHERE s.id=$1 FOR SHARE OF s,c,t")
        .bind(job.source_id).execute(&mut *send_tx).await?;
    if !active_snapshot(state, job).await?
        || !evidence::current(state, &evidence).await?
        || Utc::now().timestamp_millis() - message.create_time > MAX_AGE_MS
    {
        return Ok("scope_changed");
    }
    let answer = format!("{AGENT_PREFIX}{answer}");
    let updated = sqlx::query("UPDATE communication_takeover_jobs SET status='dispatching',answer=$2,updated_at=now() WHERE id=$1 AND status='evaluating'").bind(job.id).bind(&answer).execute(&state.pool).await?;
    if updated.rows_affected() != 1 {
        return Ok("scope_changed");
    }
    let base = &state.config.feishu.as_ref().expect("已验证配置").api_base;
    let url = reqwest::Url::parse(&format!(
        "{base}/im/v1/messages/{}/reply",
        message.message_id
    ))
    .map_err(super::super::unavailable)?;
    let sent = crate::feishu::message::send(
        &state.http,
        url,
        &token,
        crate::feishu::message::Outgoing {
            id: job.id,
            content: &answer,
            receiver: None,
            delegated: true,
        },
    )
    .await
    .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "communication_provider_rejected"))?;
    let sent_id = client::string(&sent["data"], "message_id")?;
    sqlx::query("UPDATE communication_takeover_jobs SET status='sent',sent_message_id=$2,reason=NULL,updated_at=now() WHERE id=$1 AND status='dispatching'").bind(job.id).bind(sent_id).execute(&state.pool).await?;
    sqlx::query("UPDATE communication_takeover_sessions SET last_sent_at=now(),topic=$2,version=version+1,updated_at=now() WHERE source_id=$1")
        .bind(job.source_id).bind(&topic).execute(&mut *send_tx).await?;
    sqlx::query(
        "UPDATE communication_takeover_turns SET status='closed' WHERE id=$1 AND revision=$2",
    )
    .bind(job.turn_id)
    .bind(job.turn_revision)
    .execute(&mut *send_tx)
    .await?;
    send_tx.commit().await?;
    Ok("sent")
}
/// 发送前回采最新消息进入同一轮次；出现补充、编辑或本人发言时，旧版本围栏立即失效。
async fn fresh_remote(
    state: &AppState,
    job: &Job,
    message: &Message,
    open_id: &str,
    token: &str,
) -> ApiResult<bool> {
    let response = client::json_response(client::get(state, "/im/v1/messages", token).query(&[
        ("container_id_type", "chat"),
        ("container_id", message.chat_id.as_str()),
        ("sort_type", "ByCreateTimeDesc"),
        ("page_size", RECENT_MESSAGES),
    ]))
    .await?;
    let items = response["data"]["items"]
        .as_array()
        .filter(|items| items.len() <= 20)
        .ok_or_else(|| super::super::unavailable("缺少最新消息"))?;
    let messages: Vec<Message> = items
        .iter()
        .map(|item| sync::normalize(item, &message.chat_id, open_id))
        .collect::<ApiResult<_>>()?;
    let inputs: Value =
        sqlx::query_scalar("SELECT inputs FROM communication_takeover_turns WHERE id=$1")
            .bind(job.turn_id)
            .fetch_optional(&state.pool)
            .await?
            .unwrap_or(json!([]));
    let unchanged = inputs.as_array().is_some_and(|inputs| {
        !inputs.is_empty()
            && inputs.iter().all(|original| {
                messages
                    .iter()
                    .any(|current| turns::same_input(original, &json!(current)))
            })
    });
    let _guard = state.communications.lock().await;
    let sql = format!(
        "SELECT {SOURCE_COLUMNS} FROM communication_sources WHERE id=$1 AND version=$2 AND enabled AND subscribed AND NOT removal_pending"
    );
    let source: Option<Source> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(job.source_id)
        .bind(job.source_version)
        .fetch_optional(&state.pool)
        .await?;
    if let Some(source) = source {
        turns::enqueue(state, &source, job.connection_version, &messages).await?;
    } else {
        return Ok(false);
    }
    Ok(unchanged && active(state, job).await?)
}
/// 发送资格始终来自数据库真实身份和订阅，不允许消息文本选择账号或目标会话。
async fn active(state: &AppState, job: &Job) -> ApiResult<bool> {
    // 起草期间也可能修改文件，最终发送前必须重新读取，不能只依赖数据库旧快照。
    let settings = settings::load(state).await?;
    if settings.rules_error.is_some() || settings.version != job.settings_version {
        return Ok(false);
    }
    active_snapshot(state, job).await
}
/// 已持有授权和设置行锁时只读围栏，不能再次获取设置排他锁。
async fn active_snapshot(state: &AppState, job: &Job) -> ApiResult<bool> {
    Ok(
        sqlx::query_scalar(include_str!("../../sql/takeover_active.sql"))
            .bind(job.id)
            .bind(job.source_id)
            .bind(job.source_version)
            .bind(job.connection_version)
            .bind(job.connection_generation)
            .bind(job.settings_version)
            .fetch_one(&state.pool)
            .await?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    // 验证新卡片、降级富文本及平台简化后的回采结构保留标识；不代替真实平台响应格式验收。
    #[test]
    fn rich_agent_replies_remain_excluded_after_normalization() {
        let now = Utc::now().timestamp_millis();
        for (kind, content) in [
            (
                "interactive",
                json!({"schema":"2.0","body":{"elements":[{"tag":"markdown","content":"[Agent 自动回复]","text_size":"notation"},{"tag":"markdown","content":"可以，先注册账号。"}]}}),
            ),
            (
                "interactive",
                json!({"title":"","elements":[[{"tag":"text","text":"[Agent 自动回复]"}],[{"tag":"text","text":"可以，先注册账号。"}]]}),
            ),
            (
                "post",
                json!({"zh_cn":{"title":"","content":[[{"tag":"text","text":"[Agent 自动回复]"}],[{"tag":"md","text":"可以，先 **注册账号**。"}]]}}),
            ),
        ] {
            let wire = json!({"message_id":"om_reply","chat_id":"oc_fixture","create_time":now.to_string(),"sender":{"id":"ou_owner","id_type":"open_id","sender_type":"user"},"msg_type":kind,"body":{"content":content.to_string()}});
            let message = sync::normalize(&wire, "oc_fixture", "ou_owner").unwrap();
            assert!(super::super::is_agent_message(&message.text));
            assert!(!eligible(&message, now - 100, now, true));
        }
    }

    // 仅验证本地入队资格，真实私聊可读范围与实时性仍需租户验收。
    #[test]
    fn excludes_self_history_bots_and_agent_messages() {
        let now = Utc::now().timestamp_millis();
        let mut message: Message = serde_json::from_value(json!({"message_id":"om_test","chat_id":"oc_test","sender_id":"ou_other","sender_id_type":"open_id","sender_type":"user","is_me":false,"create_time":now-10,"update_time":now-10,"message_type":"text","deleted":false,"text":"申请 novita 测试环境权限","payload":{}})).unwrap();
        assert!(eligible(&message, now - 100, now, false));
        assert!(!eligible(&message, now, now, false));
        assert!(!eligible(&message, 0, now + MAX_AGE_MS, false));
        message.is_me = true;
        assert!(!eligible(&message, 0, now, false));
        message.is_me = false;
        message.sender_type = "app".into();
        assert!(!eligible(&message, 0, now, false));
        message.sender_type = "user".into();
        // 新旧标识均排除，包含前导空白以及自聊测试放行本人消息的路径。
        for prefix in ["[agent] ", "[Agent 自动回复]\n", "  [Agent 自动回复]\n"] {
            message.text = format!("{prefix}申请 novita 测试环境权限");
            message.is_me = false;
            assert!(!eligible(&message, 0, now, false));
            message.is_me = true;
            assert!(!eligible(&message, 0, now, true));
        }
    }
}
