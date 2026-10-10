use super::{Schedule, identity, recurrence};
use crate::{AppState, error::ApiResult};
use chrono::{Duration, Utc};
use serde_json::json;
use tokio::sync::watch;
use uuid::Uuid;

// 单独创建期次，不在数据库事务里执行模型或平台网络请求。
const POLL_SECONDS: u64 = 1;
/// 每个安排到期后原子生成一次处理记录，停机恢复按跳过或补最近一期处理。
pub async fn run(state: AppState, mut stop: watch::Receiver<bool>) {
    loop {
        if *stop.borrow() {
            break;
        }
        if process_one(&state).await.is_err() {
            tracing::warn!("待办安排调度失败，将重试");
        }
        tokio::select! { _=tokio::time::sleep(std::time::Duration::from_secs(POLL_SECONDS))=>{}, _=stop.changed()=>{} }
    }
}
/// 先锁安排、再生成期次并推进时间，事务回滚不会吞掉本次触发。
pub async fn process_one(state: &AppState) -> ApiResult<bool> {
    let mut tx = state.pool.begin().await?;
    // 与编辑安排使用相同主体锁，避免完成和派发交错生成孤立期次。
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('orbit:todos:admin'))")
        .execute(&mut *tx)
        .await?;
    let row:Option<Schedule>=sqlx::query_as("SELECT s.* FROM todo_schedules s JOIN todos t ON t.id=s.todo_id WHERE s.status='enabled' AND s.next_run_at<=now() AND t.status IN('active','waiting_external','needs_user') ORDER BY s.next_run_at,s.id FOR UPDATE OF s SKIP LOCKED LIMIT 1")
        .fetch_optional(&mut *tx).await?;
    let Some(s) = row else { return Ok(false) };
    let binding_valid = if s.delivery_owner == "admin" {
        true
    } else {
        let v: Option<i64> = sqlx::query_scalar(
            "SELECT version FROM personal_identities WHERE owner=$1 AND enabled",
        )
        .bind(&s.delivery_owner)
        .fetch_optional(&mut *tx)
        .await?;
        v.is_some() && v == s.binding_version
    };
    let owner: String = sqlx::query_scalar("SELECT owner FROM todos WHERE id=$1")
        .bind(s.todo_id)
        .fetch_one(&mut *tx)
        .await?;
    let principal = identity::principal(state, &s.delivery_owner).await?;
    if !binding_valid
        || principal != owner
        || !crate::followups::policy::owner_allowed(state, &s.delivery_owner).await?
    {
        sqlx::query("UPDATE todo_schedules SET status='paused',version=version+1,updated_at=now() WHERE id=$1").bind(s.id).execute(&mut *tx).await?;
        super::service::event(
            &mut tx,
            s.todo_id,
            "system",
            "schedule_paused",
            json!({"id":s.id,"reason":"identity_changed"}),
        )
        .await?;
        tx.commit().await?;
        return Ok(true);
    }
    let now = Utc::now();
    let next = recurrence::next(s.anchor_at, now, &s.timezone, &s.recurrence)?;
    let next = next.filter(|n| s.ends_at.is_none_or(|end| *n <= end));
    let mut occurrence = s.next_run_at;
    if s.missed_policy == "latest" && s.recurrence != "once" {
        for _ in 0..3660 {
            match recurrence::next(s.anchor_at, occurrence, &s.timezone, &s.recurrence)? {
                Some(next) if next <= now => occurrence = next,
                _ => break,
            }
        }
    }
    let expired = s.ends_at.is_some_and(|end| now > end);
    let too_late = now > occurrence + Duration::minutes(i64::from(s.grace_minutes));
    let missed = recurrence::next(s.anchor_at, s.next_run_at, &s.timezone, &s.recurrence)?
        .is_some_and(|n| n <= now);
    if expired || too_late || (s.missed_policy == "skip" && missed) {
        sqlx::query("INSERT INTO todo_runs(id,todo_id,schedule_id,schedule_version,scheduled_at,status,error,finished_at) VALUES($1,$2,$3,$4,$5,'skipped',$6,now()) ON CONFLICT DO NOTHING")
            .bind(Uuid::new_v4()).bind(s.todo_id).bind(s.id).bind(s.version).bind(occurrence).bind(if expired {"schedule_ended"} else {"missed_window"}).execute(&mut *tx).await?;
    } else {
        let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM todo_runs WHERE schedule_id=$1 AND schedule_version=$2 AND scheduled_at=$3)")
            .bind(s.id).bind(s.version).bind(occurrence).fetch_one(&mut *tx).await?;
        if !exists {
            sqlx::query("INSERT INTO followup_preferences(owner,timezone) VALUES($1,$2) ON CONFLICT DO NOTHING").bind(&principal).bind(&s.timezone).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO followup_preferences(owner,timezone) VALUES($1,$2) ON CONFLICT DO NOTHING").bind(&s.delivery_owner).bind(&s.timezone).execute(&mut *tx).await?;
            let title: String = sqlx::query_scalar("SELECT title FROM todos WHERE id=$1")
                .bind(s.todo_id)
                .fetch_one(&mut *tx)
                .await?;
            // 明确提醒直接携带用户保存的说明；回访与整理在各自模型上下文中读取说明。
            let topic = if s.kind == "reminder" && !s.instruction.trim().is_empty() {
                format!("{}：{}", title, s.instruction)
            } else {
                title
            };
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO followups(id,owner,conversation_id,kind,topic,due_at,expires_at,timezone,origin_key,request_hash,todo_schedule_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,'todo-schedule',$10)")
                .bind(id).bind(&s.delivery_owner).bind(s.conversation_id).bind(if s.kind=="checkin" {"checkin"} else {"reminder"}).bind(topic).bind(occurrence).bind(occurrence+Duration::minutes(i64::from(s.grace_minutes))).bind(&s.timezone).bind(format!("todo:{}:{}:{}",s.id,s.version,occurrence.timestamp_micros())).bind(s.id).execute(&mut *tx).await?;
        }
    }
    sqlx::query("UPDATE todo_schedules SET status=$2,next_run_at=COALESCE($3,next_run_at),updated_at=now() WHERE id=$1")
        .bind(s.id).bind(if next.is_some() && !expired {"enabled"} else {"ended"}).bind(next).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(true)
}
/// 每次投递与工具读取重验业务状态、安排版本和身份；已结束的一次性安排仍可投递其最后一期。
pub async fn delivery_allowed(state: &AppState, followup: Uuid) -> ApiResult<bool> {
    Ok(sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM followups f JOIN todo_schedules s ON s.id=f.todo_schedule_id JOIN todos t ON t.id=s.todo_id JOIN todo_runs r ON r.followup_id=f.id WHERE f.id=$1 AND (t.status IN('completed','cancelled') OR s.status='paused' OR s.version<>r.schedule_version OR (t.owner='admin' AND (personal_owner(s.delivery_owner)<>'admin' OR (s.delivery_owner<>'admin' AND NOT EXISTS(SELECT 1 FROM personal_identities i WHERE i.owner=s.delivery_owner AND i.enabled AND i.version=s.binding_version))))))")
        .bind(followup).fetch_one(&state.pool).await?)
}
/// 明确单项回访授权不要求开启全局自动发现；旧版回访仍遵守其全局开关。
pub async fn explicit_checkin(state: &AppState, followup: Uuid) -> ApiResult<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM followups f JOIN todo_schedules s ON s.id=f.todo_schedule_id JOIN todos t ON t.id=s.todo_id WHERE f.id=$1 AND s.kind='checkin' AND t.origin_key NOT LIKE 'followup:%')")
        .bind(followup).fetch_one(&state.pool).await?)
}
/// 返回跨渠道事项快照作为回访依据，模型只能提出本次处理建议。
pub async fn context(state: &AppState, followup: Uuid) -> ApiResult<serde_json::Value> {
    let value:Option<serde_json::Value>=sqlx::query_scalar("SELECT jsonb_build_object('item',to_jsonb(t),'instruction',s.instruction,'timezone',s.timezone) FROM todos t JOIN todo_schedules s ON s.todo_id=t.id JOIN followups f ON f.todo_schedule_id=s.id WHERE f.id=$1")
        .bind(followup).fetch_optional(&state.pool).await?;
    Ok(value.unwrap_or(serde_json::Value::Null))
}
