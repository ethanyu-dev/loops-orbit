use super::{COLUMNS, Followup, policy, preferences};
use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::json;
use sqlx::{Postgres, Transaction};
use tokio::sync::watch;
use uuid::Uuid;

// 回访留出用户正在交流的时间；到期 worker 与发现 worker 分离。
const ACTIVE_GRACE_MINUTES: i64 = 15;
const MAX_ATTEMPTS: i32 = 3;
const CLAIM: &str = include_str!("../sql/followup_claim.sql");

/// 模型仅决定回访时机和文案，不能改变接收者或直接发出消息。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    /// send/defer/complete/cancel。
    decision: String,
    /// 只有 send 需要，最多 300 字。
    message: Option<String>,
}
/// 每条通道独立轮询，关闭时放弃继续领取，未完成工作按租约恢复。
pub async fn run(state: AppState, kind: &'static str, mut stop: watch::Receiver<bool>) {
    loop {
        if *stop.borrow() {
            break;
        }
        if process_one(&state, kind).await.is_err() {
            tracing::warn!(kind, "跟进调度失败，等待租约恢复");
        }
        tokio::select! {_=tokio::time::sleep(std::time::Duration::from_secs(1))=>{},_=stop.changed()=>{}}
    }
}
/// 原子领取后可以恢复；同一版本的正式消息只生成一次。
pub async fn process_one(state: &AppState, kind: &str) -> ApiResult<bool> {
    sqlx::query("UPDATE followups SET status='expired',lease_token=NULL,updated_at=now() WHERE status IN('scheduled','checking','queued') AND expires_at<=now()")
        .execute(&state.pool).await?;
    sqlx::query(include_str!("../sql/followup_clean_outbox.sql"))
        .execute(&state.pool)
        .await?;
    // 未完成或取消的用户轮次不能留下会在未来自行触发的候选。
    sqlx::query("UPDATE followups SET status='cancelled',version=version+1,lease_token=NULL,updated_at=now() WHERE status IN('scheduled','checking') AND source_run_id IN(SELECT id FROM runs WHERE status IN('cancelled','superseded','failed'))")
        .execute(&state.pool).await?;
    let job: Option<Followup> = sqlx::query_as(CLAIM)
        .bind(kind)
        .bind(Uuid::new_v4())
        .fetch_optional(&state.pool)
        .await?;
    let Some(mut job) = job else { return Ok(false) };
    if job.attempts > MAX_ATTEMPTS {
        finish(state, &job, "failed", Some("followup_attempt_limit")).await?;
        return Ok(true);
    }
    if job.expires_at <= Utc::now() {
        finish(state, &job, "expired", None).await?;
        return Ok(true);
    }
    if !policy::owner_allowed(state, &job.owner).await? {
        finish(state, &job, "cancelled", Some("followup_owner_unavailable")).await?;
        return Ok(true);
    }
    if !policy::dependencies_valid(state, &job).await? {
        finish(state, &job, "cancelled", Some("memory_changed")).await?;
        return Ok(true);
    }
    if !crate::todos::scheduler::delivery_allowed(state, job.id).await? {
        finish(state, &job, "cancelled", Some("todo_schedule_changed")).await?;
        return Ok(true);
    }
    if job.kind == "reminder" {
        match crate::todos::execution::generate(state, &job).await {
            Ok(Some(text)) => queue(state, &job, &text, None).await?,
            Ok(None) => queue(state, &job, &format!("提醒你：{}", job.topic), None).await?,
            Err(_) => retry(state, &job).await?,
        }
        return Ok(true);
    }
    let prefs = preferences(state, &job.owner).await?;
    if !prefs.enabled && !crate::todos::scheduler::explicit_checkin(state, job.id).await? {
        finish(state, &job, "cancelled", None).await?;
        return Ok(true);
    }
    if policy::is_quiet(&prefs, Utc::now()) {
        defer(state, &job, policy::next_awake(&prefs, Utc::now())).await?;
        return Ok(true);
    }
    let (seq,last): (i64,Option<DateTime<Utc>>)=sqlx::query_as("SELECT COALESCE(max(seq),0),max(created_at) FROM runs WHERE conversation_id IN(SELECT id FROM conversations WHERE personal_owner(owner)=personal_owner($1))")
        .bind(&job.owner).fetch_one(&state.pool).await?;
    if last.is_some_and(|t| t > Utc::now() - Duration::minutes(ACTIVE_GRACE_MINUTES)) {
        defer(
            state,
            &job,
            Utc::now() + Duration::minutes(ACTIVE_GRACE_MINUTES),
        )
        .await?;
        return Ok(true);
    }
    let messages: Vec<(String, String)> =
        sqlx::query_as(include_str!("../sql/followup_context.sql"))
            .bind(job.conversation_id)
            .bind(&job.owner)
            .fetch_all(&state.pool)
            .await?;
    let history: Vec<agent_runtime::Message> = messages
        .into_iter()
        .rev()
        .map(|(role, content)| agent_runtime::Message { role, content })
        .collect();
    let memory = if let Some(config) = &state.config.memory {
        let mut used = crate::memory::list(state, &job.owner)
            .await?
            .into_iter()
            .filter(|e| e.active() && e.kind == "profile")
            .take(4)
            .collect::<Vec<_>>();
        for entry in crate::memory::search(state, &job.owner, &job.topic).await? {
            if !used.iter().any(|e| e.id == entry.id) {
                used.push(entry);
            }
        }
        let _guard = state.memory.lock().await;
        if !policy::dependencies_valid(state, &job).await? {
            finish(state, &job, "cancelled", Some("memory_changed")).await?;
            return Ok(true);
        }
        let current = crate::memory::store::read(&config.directory, &job.owner)
            .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "memory_unavailable"))?;
        if !used.iter().all(|old| {
            current
                .iter()
                .any(|new| new.active() && new.id == old.id && new.hash() == old.hash())
        }) {
            defer(state, &job, Utc::now() + Duration::hours(1)).await?;
            return Ok(true);
        }
        for entry in &used {
            if !job.memory_ids.contains(&entry.id) {
                job.memory_ids.push(entry.id);
            }
            job.memory_versions[entry.id.to_string()] = json!(entry.hash());
        }
        // 和遗忘共用文件写锁，绑定依赖后才允许模型持有原文快照。
        let bound = sqlx::query("UPDATE followups SET memory_ids=$4,memory_versions=$5 WHERE id=$1 AND version=$2 AND lease_token=$3 AND status='checking'")
            .bind(job.id).bind(job.version).bind(job.lease_token).bind(&job.memory_ids).bind(&job.memory_versions).execute(&state.pool).await?;
        // 资料准备期间若发生遗忘或取消，连模型判断也不再启动。
        if bound.rows_affected() == 0 {
            return Ok(true);
        }
        json!(used)
    } else {
        json!([])
    };
    let todo_context = crate::todos::scheduler::context(state, job.id).await?;
    let decision = tokio::time::timeout(
        std::time::Duration::from_secs(35),
        state.runtime.followup_decision(
            &json!({"now":Utc::now(),"followup":job,"recent_messages":history,"memory":memory,"communication":job.memory_versions["_communication"],"todo":todo_context}),
            false,
        ),
    )
    .await;
    let decision = match decision {
        Ok(Ok(value)) => serde_json::from_value::<Decision>(value).ok(),
        _ => None,
    };
    match decision {
        Some(Decision { decision, message }) if decision == "send" => {
            if let Some(text) = message.filter(|s| !s.trim().is_empty() && s.chars().count() <= 300)
            {
                queue(state, &job, &text, Some(seq)).await?;
            } else {
                retry(state, &job).await?;
            }
        }
        Some(Decision { decision, .. }) if decision == "complete" || decision == "cancel" => {
            // 同来源会话输入提交与终止判断共用行锁，旧模型不能结束更新后的事项。
            let mut tx = state.pool.begin().await?;
            sqlx::query("SELECT id FROM conversations WHERE personal_owner(owner)=personal_owner($1) ORDER BY id FOR UPDATE")
                .bind(&job.owner)
                .execute(&mut *tx)
                .await?;
            let unchanged: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM runs WHERE conversation_id IN(SELECT id FROM conversations WHERE personal_owner(owner)=personal_owner($1)) AND seq>$2)").bind(&job.owner).bind(seq).fetch_one(&mut *tx).await?;
            if unchanged {
                sqlx::query("UPDATE followups SET status=$4,lease_token=NULL,lease_until=NULL,updated_at=now() WHERE id=$1 AND version=$2 AND lease_token=$3 AND status='checking'")
                    .bind(job.id).bind(job.version).bind(job.lease_token).bind(if decision == "complete" { "completed" } else { "cancelled" }).execute(&mut *tx).await?;
                tx.commit().await?;
            } else {
                tx.rollback().await?;
                defer(
                    state,
                    &job,
                    Utc::now() + Duration::minutes(ACTIVE_GRACE_MINUTES),
                )
                .await?;
            }
        }
        Some(Decision { decision, .. }) if decision == "defer" => {
            defer(state, &job, Utc::now() + Duration::hours(6)).await?
        }
        _ => retry(state, &job).await?,
    }
    Ok(true)
}
/// 完成/取消只修改仍持有租约的版本。
async fn finish(
    state: &AppState,
    job: &Followup,
    status: &str,
    error: Option<&str>,
) -> ApiResult<()> {
    sqlx::query("UPDATE followups SET status=$4,error=$5,lease_token=NULL,lease_until=NULL,updated_at=now() WHERE id=$1 AND version=$2 AND lease_token=$3 AND status='checking'")
        .bind(job.id).bind(job.version).bind(job.lease_token).bind(status).bind(error).execute(&state.pool).await?;
    Ok(())
}
/// 延后不会消耗故障重试次数，到期后结束而非无限追问。
async fn defer(state: &AppState, job: &Followup, due: DateTime<Utc>) -> ApiResult<()> {
    if due >= job.expires_at {
        return finish(state, job, "expired", None).await;
    }
    sqlx::query("UPDATE followups SET status='scheduled',due_at=$4,attempts=0,lease_token=NULL,lease_until=NULL,updated_at=now() WHERE id=$1 AND version=$2 AND lease_token=$3 AND status='checking'")
        .bind(job.id).bind(job.version).bind(job.lease_token).bind(due).execute(&state.pool).await?;
    Ok(())
}
/// 模型失败保持原事项，最多三次，不伪装成用户完成。
async fn retry(state: &AppState, job: &Followup) -> ApiResult<()> {
    if job.attempts >= MAX_ATTEMPTS {
        return finish(state, job, "failed", Some("followup_model_failed")).await;
    }
    sqlx::query("UPDATE followups SET status='scheduled',due_at=LEAST(now()+interval '1 minute',expires_at-interval '1 millisecond'),lease_token=NULL,lease_until=NULL,error='followup_model_failed' WHERE id=$1 AND version=$2 AND lease_token=$3")
        .bind(job.id).bind(job.version).bind(job.lease_token).execute(&state.pool).await?;
    Ok(())
}
/// 排队与网页消息写入同事务；发送前持有会话、身份频率和事项版本锁。
pub async fn queue(
    state: &AppState,
    job: &Followup,
    text: &str,
    seen_seq: Option<i64>,
) -> ApiResult<()> {
    let mut tx = state.pool.begin().await?;
    // 与待办完成、暂停及解绑共用锁，校验通过后不能被旧投递覆盖取消。
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('orbit:todos:admin'))")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT id FROM conversations WHERE personal_owner(owner)=personal_owner($1) ORDER BY id FOR UPDATE")
        .bind(&job.owner)
        .execute(&mut *tx)
        .await?;
    let canonical = crate::todos::identity::principal(state, &job.owner).await?;
    sqlx::query("SELECT owner FROM followup_preferences WHERE owner=$1 FOR UPDATE")
        .bind(&canonical)
        .execute(&mut *tx)
        .await?;
    let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM followups WHERE id=$1 AND version=$2 AND lease_token=$3 AND status='checking' AND expires_at>now())")
        .bind(job.id).bind(job.version).bind(job.lease_token).fetch_one(&mut *tx).await?;
    if !active {
        return Ok(());
    }
    if !can_deliver(state, job, seen_seq).await? {
        tx.rollback().await?;
        return defer(state, job, Utc::now() + Duration::minutes(60)).await;
    }
    let channel: String = sqlx::query_scalar("SELECT channel FROM conversations WHERE id=$1")
        .bind(job.conversation_id)
        .fetch_one(&mut *tx)
        .await?;
    if channel == "feishu" {
        let receiver = job
            .owner
            .strip_prefix("feishu:")
            .ok_or(ApiError(StatusCode::CONFLICT, "followup_owner_unavailable"))?;
        sqlx::query("INSERT INTO outbox(id,receiver_id,followup_id,followup_version,content) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING")
            .bind(Uuid::new_v4()).bind(receiver).bind(job.id).bind(job.version).bind(text).execute(&mut *tx).await?;
        sqlx::query("UPDATE followups SET status='queued',observed_seq=$2,lease_token=NULL,lease_until=NULL,updated_at=now() WHERE id=$1").bind(job.id).bind(seen_seq).execute(&mut *tx).await?;
    } else {
        acknowledge(&mut tx, job, text).await?;
    }
    tx.commit().await?;
    Ok(())
}
/// 投递时重复检查静默、授权、频率和用户活动，排队不意味着必须发送。
pub async fn can_deliver(
    state: &AppState,
    job: &Followup,
    seen_seq: Option<i64>,
) -> ApiResult<bool> {
    if !crate::todos::execution::dependencies_valid(state, job.id).await? {
        return Ok(false);
    }
    if !crate::todos::scheduler::delivery_allowed(state, job.id).await?
        || !policy::owner_allowed(state, &job.owner).await?
        || job.expires_at <= Utc::now()
    {
        return Ok(false);
    }
    if !policy::dependencies_valid(state, job).await? {
        return Ok(false);
    }
    if job.kind == "reminder" {
        return Ok(true);
    }
    // 此处可能已有偏好行锁，只读快照，不能在另一连接尝试插入同一行。
    let prefs: super::Preferences = sqlx::query_as("SELECT timezone,enabled,quiet_start,quiet_end,min_interval_minutes,version FROM followup_preferences WHERE owner=personal_owner($1)").bind(&job.owner).fetch_one(&state.pool).await?;
    if (!prefs.enabled && !crate::todos::scheduler::explicit_checkin(state, job.id).await?)
        || policy::is_quiet(&prefs, Utc::now())
    {
        return Ok(false);
    }
    let suppressed: bool = sqlx::query_scalar(include_str!("../sql/followup_suppressed.sql"))
        .bind(&job.owner)
        .bind(prefs.min_interval_minutes)
        .bind(job.id)
        .bind(seen_seq.or(job.observed_seq))
        .fetch_one(&state.pool)
        .await?;
    Ok(!suppressed)
}
/// 只在确认实际发送后写入助手主动消息，来源与普通用户轮次完全分开。
pub async fn acknowledge(
    tx: &mut Transaction<'_, Postgres>,
    job: &Followup,
    text: &str,
) -> ApiResult<()> {
    sqlx::query("INSERT INTO messages(conversation_id,role,content,kind,followup_id,followup_version) VALUES($1,'assistant',$2,'followup',$3,$4) ON CONFLICT DO NOTHING")
        .bind(job.conversation_id).bind(text).bind(job.id).bind(job.version).execute(&mut **tx).await?;
    sqlx::query("UPDATE followups SET status='sent',sent_at=now(),lease_token=NULL,lease_until=NULL,error=NULL,updated_at=now() WHERE id=$1 AND version=$2")
        .bind(job.id).bind(job.version).execute(&mut **tx).await?;
    if job.kind == "checkin" {
        sqlx::query(
            "UPDATE followup_preferences SET last_checkin_at=now() WHERE owner=personal_owner($1)",
        )
        .bind(&job.owner)
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query("UPDATE conversations SET updated_at=now() WHERE id=$1")
        .bind(job.conversation_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
/// 飞书领取后、调用 HTTP 前验证版本；调用已开始时取消只能阻止后续重试。
pub async fn prepare_delivery(
    state: &AppState,
    id: Uuid,
    version: i64,
) -> ApiResult<Option<Followup>> {
    let job: Option<Followup> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM followups WHERE id=$1 AND version=$2 AND status='queued'"
    )))
    .bind(id)
    .bind(version)
    .fetch_optional(&state.pool)
    .await?;
    let Some(job) = job else { return Ok(None) };
    if !can_deliver(state, &job, None).await? {
        let mut tx = state.pool.begin().await?;
        sqlx::query("SELECT id FROM conversations WHERE id=$1 FOR UPDATE")
            .bind(job.conversation_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE outbox SET status='cancelled' WHERE followup_id=$1 AND followup_version=$2 AND status IN('queued','running')").bind(id).bind(version).execute(&mut *tx).await?;
        let cancelled = !crate::todos::scheduler::delivery_allowed(state, job.id).await?
            || !policy::dependencies_valid(state, &job).await?
            || !policy::owner_allowed(state, &job.owner).await?
            || (job.kind == "checkin"
                && !preferences(state, &job.owner).await?.enabled
                && !crate::todos::scheduler::explicit_checkin(state, job.id).await?);
        // 废弃该队列身份，新一次判断使用新版本，避免重用旧正文。
        sqlx::query(include_str!("../sql/followup_reconsider.sql"))
            .bind(id)
            .bind(version)
            .bind(cancelled)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(None);
    }
    Ok(Some(job))
}
