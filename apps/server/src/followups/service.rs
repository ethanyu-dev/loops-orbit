use super::{COLUMNS, Create, Followup, Preferences, Source, Update, policy};
use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

// 每身份上限防止意外模型循环创建大量长期任务。
const MAX_ACTIVE: i64 = 100;
const CANCEL_OUTBOX: &str = include_str!("../sql/followup_cancel_outbox.sql");

/// 偏好行同时作为同一身份创建配额和回访发送频率的串行锁。
pub async fn preferences(state: &AppState, owner: &str) -> ApiResult<Preferences> {
    // 来源行保留给旧提醒外键，实际偏好按本人主体共享。
    sqlx::query(
        "INSERT INTO followup_preferences(owner,timezone) VALUES($1,$2) ON CONFLICT DO NOTHING",
    )
    .bind(owner)
    .bind(&state.config.followup_timezone)
    .execute(&state.pool)
    .await?;
    let canonical = crate::todos::identity::principal(state, owner).await?;
    let owner = canonical.as_str();
    sqlx::query(
        "INSERT INTO followup_preferences(owner,timezone) VALUES($1,$2) ON CONFLICT DO NOTHING",
    )
    .bind(owner)
    .bind(&state.config.followup_timezone)
    .execute(&state.pool)
    .await?;
    Ok(sqlx::query_as("SELECT timezone,enabled,quiet_start,quiet_end,min_interval_minutes,version FROM followup_preferences WHERE owner=$1")
        .bind(owner).fetch_one(&state.pool).await?)
}
/// 关闭回访会取消已计划与排队中的回访；明确提醒继续保留。
pub async fn save_preferences(state: &AppState, owner: &str, prefs: &Preferences) -> ApiResult<()> {
    save_preferences_with_source(state, owner, prefs, None).await
}
/// 聊天修改将来源租约与偏好变更放在同一事务，防止旧轮次覆盖新决定。
pub(super) async fn save_preferences_with_source(
    state: &AppState,
    owner: &str,
    prefs: &Preferences,
    source: Option<&Source<'_>>,
) -> ApiResult<()> {
    let canonical = crate::todos::identity::principal(state, owner).await?;
    let owner = canonical.as_str();
    policy::validate(prefs)?;
    preferences(state, owner).await?;
    let mut tx = state.pool.begin().await?;
    // 与待办调度统一锁序，避免会话锁和事项锁交叉等待。
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('orbit:todos:admin'))")
        .execute(&mut *tx)
        .await?;
    // 与遗忘采用相同会话锁顺序，避免关闭偏好与在途判断互相覆盖。
    sqlx::query(
        "SELECT id FROM conversations WHERE personal_owner(owner)=$1 ORDER BY id FOR UPDATE",
    )
    .bind(owner)
    .execute(&mut *tx)
    .await?;
    if source_fence(&mut tx, source).await?.is_some() {
        return Ok(());
    }
    // 只有明确从开启切到关闭才停止回访；关闭状态下调整静默时间不撤销单项授权。
    let was_enabled: bool =
        sqlx::query_scalar("SELECT enabled FROM followup_preferences WHERE owner=$1 FOR UPDATE")
            .bind(owner)
            .fetch_one(&mut *tx)
            .await?;
    let changed=sqlx::query("UPDATE followup_preferences SET timezone=$2,enabled=$3,quiet_start=$4,quiet_end=$5,min_interval_minutes=$6,version=version+1 WHERE owner=$1 AND version=$7")
        .bind(owner).bind(&prefs.timezone).bind(prefs.enabled).bind(prefs.quiet_start).bind(prefs.quiet_end).bind(prefs.min_interval_minutes).bind(prefs.version).execute(&mut *tx).await?;
    if changed.rows_affected() == 0 {
        return Err(ApiError(StatusCode::CONFLICT, "followup_version_conflict"));
    }
    if was_enabled && !prefs.enabled {
        sqlx::query("UPDATE todo_schedules SET status='paused',version=version+1 WHERE kind='checkin' AND todo_id IN(SELECT id FROM todos WHERE owner=$1)").bind(owner).execute(&mut *tx).await?;
        sqlx::query(
            "UPDATE followup_discovery SET status='completed' WHERE personal_owner(owner)=$1 AND status='queued'",
        )
        .bind(owner)
        .execute(&mut *tx)
        .await?;
        let ids:Vec<Uuid>=sqlx::query_scalar("UPDATE followups SET status='cancelled',version=version+1,lease_token=NULL,updated_at=now() WHERE personal_owner(owner)=$1 AND kind='checkin' AND status IN('scheduled','checking','queued') RETURNING id")
            .bind(owner).fetch_all(&mut *tx).await?;
        for id in ids {
            sqlx::query(CANCEL_OUTBOX)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
    }
    record(&mut tx, source, &json!({"enabled": prefs.enabled})).await?;
    tx.commit().await?;
    Ok(())
}
/// 服务端从来源任务确定 owner 和租约，模型不能用旧调用修改新状态。
async fn source_fence(
    tx: &mut Transaction<'_, Postgres>,
    source: Option<&Source<'_>>,
) -> ApiResult<Option<Value>> {
    let Some(source) = source else {
        return Ok(None);
    };
    let active: bool = sqlx::query_scalar(include_str!("../sql/followup_source_fence.sql"))
        .bind(source.job.id)
        .bind(source.job.lease_token)
        .bind(source.discovery_attempt)
        .fetch_one(&mut **tx)
        .await?;
    if !active {
        return Err(ApiError(StatusCode::CONFLICT, "run_superseded"));
    }
    Ok(sqlx::query_scalar(
        "SELECT result FROM followup_operations WHERE run_id=$1 AND operation_key=$2",
    )
    .bind(source.job.id)
    .bind(&source.operation_key)
    .fetch_optional(&mut **tx)
    .await?)
}
/// 动作结果与状态变更同事务保存，模型超时重试只会读回原结果。
async fn record(
    tx: &mut Transaction<'_, Postgres>,
    source: Option<&Source<'_>>,
    result: &Value,
) -> ApiResult<()> {
    if let Some(source) = source {
        sqlx::query("INSERT INTO followup_operations(run_id,operation_key,result) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
            .bind(source.job.id).bind(&source.operation_key).bind(result).execute(&mut **tx).await?;
    }
    Ok(())
}
/// 手工创建使用独立提醒会话；聊天工具只能使用本轮来源会话。
pub async fn create(
    state: &AppState,
    owner: &str,
    input: &Create,
    source: Option<&Source<'_>>,
) -> ApiResult<Value> {
    if !policy::owner_allowed(state, owner).await? {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "followup_owner_unavailable",
        ));
    }
    let request_hash = crate::auth::hash(&serde_json::to_string(input).expect("创建参数可序列化"));
    // 网页断网重试即使已越过到期时刻，也应读回同一动作；不同正文不能借用旧键。
    if source.is_none() {
        let previous: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT id,request_hash FROM followups WHERE owner=$1 AND origin_key=$2",
        )
        .bind(owner)
        .bind(input.idempotency_key.to_string())
        .fetch_optional(&state.pool)
        .await?;
        if let Some((id, hash)) = previous {
            if hash != request_hash {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    "followup_idempotency_conflict",
                ));
            }
            return Ok(json!({"id": id, "already_exists": true}));
        }
    }
    let prefs = preferences(state, owner).await?;
    if !matches!(input.kind.as_str(), "reminder" | "checkin")
        || input.topic.trim().is_empty()
        || input.topic.chars().count() > policy::MAX_TOPIC_CHARS
        || input.memory_ids.len() > 20
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_followup"));
    }
    let due = policy::parse_time(&input.due_at, &prefs.timezone)?;
    let expiry = input
        .expires_at
        .as_ref()
        .map(|s| policy::parse_time(s, &prefs.timezone))
        .transpose()?
        .unwrap_or(due + Duration::days(if input.kind == "reminder" { 1 } else { 3 }));
    if due <= Utc::now()
        || due > Utc::now() + Duration::days(policy::MAX_HORIZON_DAYS)
        || expiry <= due
        || expiry > due + Duration::days(7)
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_followup_time"));
    }
    // 原文依赖验证与插入共用文件锁，遗忘不能从验证和绑定之间穿过。
    let mut memory_versions = json!({});
    let _communication_guard = if let Some(reference) = &input.communication {
        if source.is_some() {
            return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_followup"));
        }
        let guard = state.communications.lock().await;
        memory_versions["_communication"] =
            crate::communications::dependencies::bind(state, owner, reference).await?;
        Some(guard)
    } else {
        None
    };
    let _memory_guard = if input.memory_ids.is_empty() {
        None
    } else {
        let config = state
            .config
            .memory
            .as_ref()
            .ok_or(ApiError(StatusCode::CONFLICT, "memory_disabled"))?;
        let guard = state.memory.lock().await;
        let entries = crate::memory::store::read(&config.directory, owner)
            .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "memory_unavailable"))?;
        if !input.memory_ids.iter().all(|id| {
            entries
                .iter()
                .any(|entry| entry.id == *id && entry.active())
        }) {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                "invalid_memory_dependency",
            ));
        }
        for entry in entries
            .iter()
            .filter(|entry| input.memory_ids.contains(&entry.id))
        {
            memory_versions[entry.id.to_string()] = json!(entry.hash());
        }
        Some(guard)
    };
    let mut tx = state.pool.begin().await?;
    // 与待办调度统一锁序，避免会话锁和事项锁交叉等待。
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('orbit:todos:admin'))")
        .execute(&mut *tx)
        .await?;
    let conversation = if let Some(id) = input.conversation_id {
        let found: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM conversations WHERE id=$1 AND owner=$2 FOR UPDATE")
                .bind(id)
                .bind(owner)
                .fetch_optional(&mut *tx)
                .await?;
        found.ok_or(ApiError(StatusCode::NOT_FOUND, "conversation_not_found"))?
    } else {
        if source.is_some() {
            return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_followup"));
        }
        sqlx::query_scalar("INSERT INTO conversations(id,owner,channel,external_key,title) VALUES($1,$2,'web',$3,'提醒与跟进') ON CONFLICT(external_key) DO UPDATE SET external_key=excluded.external_key RETURNING id")
            .bind(Uuid::new_v4()).bind(owner).bind(format!("followups:{owner}")).fetch_one(&mut *tx).await?
    };
    if let Some(source) = source
        && conversation != source.job.conversation_id
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_followup"));
    }
    if let Some(result) = source_fence(&mut tx, source).await? {
        return Ok(result);
    }
    let canonical = crate::todos::identity::principal(state, owner).await?;
    let (enabled, pref_version): (bool, i64) = sqlx::query_as(
        "SELECT enabled,version FROM followup_preferences WHERE owner=$1 FOR UPDATE",
    )
    .bind(&canonical)
    .fetch_one(&mut *tx)
    .await?;
    if pref_version != prefs.version {
        return Err(ApiError(StatusCode::CONFLICT, "followup_version_conflict"));
    }
    if input.kind == "checkin" && !enabled {
        return Err(ApiError(StatusCode::CONFLICT, "checkins_disabled"));
    }
    let origin = source
        .map(|s| format!("{}:{}", s.job.id, s.operation_key))
        .unwrap_or(input.idempotency_key.to_string());
    let previous: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id,request_hash FROM followups WHERE owner=$1 AND origin_key=$2")
            .bind(owner)
            .bind(&origin)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some((id, hash)) = previous {
        if source.is_none() && hash != request_hash {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "followup_idempotency_conflict",
            ));
        }
        return Ok(json!({"id":id,"already_exists":true}));
    }
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM followups WHERE owner=$1 AND status IN('scheduled','checking','queued')").bind(owner).fetch_one(&mut *tx).await?;
    if count >= MAX_ACTIVE {
        return Err(ApiError(StatusCode::CONFLICT, "followup_limit"));
    }
    let id = Uuid::new_v4();
    sqlx::query(include_str!("../sql/followup_insert.sql"))
        .bind(id)
        .bind(owner)
        .bind(conversation)
        .bind(&input.kind)
        .bind(input.topic.trim())
        .bind(due)
        .bind(expiry)
        .bind(&prefs.timezone)
        .bind(origin)
        .bind(source.map(|s| s.job.id))
        .bind(source.map_or(0, |s| s.job.seq))
        .bind(&input.memory_ids)
        .bind(memory_versions)
        .bind(request_hash)
        .execute(&mut *tx)
        .await?;
    // 自动发现只产生待确认候选，与创建同事务提交，重试不会覆盖之后的用户确认。
    let candidate = canonical == "admin" && source.is_some_and(|s| s.discovery_attempt.is_some());
    if candidate {
        sqlx::query("UPDATE todos SET status='needs_user',next_action='确认是否需要主动回访',version=version+1 WHERE id=$1").bind(id).execute(&mut *tx).await?;
        sqlx::query("UPDATE todo_schedules SET status='paused',version=version+1 WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        crate::todos::service::cancel_pending(&mut tx, id).await?;
    }
    let result = json!({"id":id,"version":if candidate {2} else {1},"status":if candidate {"cancelled"} else {"scheduled"},"due_at":due,"expires_at":expiry,"timezone":prefs.timezone,"topic":input.topic,"conversation_id":conversation});
    record(&mut tx, source, &result).await?;
    tx.commit().await?;
    Ok(result)
}
/// 改期产生新版本和新投递身份；已发出的消息保留可查看历史。
pub async fn update(
    state: &AppState,
    owner: &str,
    id: Uuid,
    input: &Update,
    source: Option<&Source<'_>>,
) -> ApiResult<Value> {
    if !matches!(
        input.status.as_str(),
        "scheduled" | "cancelled" | "completed"
    ) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_followup"));
    }
    let prefs = preferences(state, owner).await?;
    let due = input
        .due_at
        .as_ref()
        .map(|s| policy::parse_time(s, &prefs.timezone))
        .transpose()?;
    if input.status == "scheduled"
        && due.is_none_or(|d| {
            d <= Utc::now() || d > Utc::now() + Duration::days(policy::MAX_HORIZON_DAYS)
        })
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_followup_time"));
    }
    if input
        .topic
        .as_ref()
        .is_some_and(|s| s.trim().is_empty() || s.chars().count() > policy::MAX_TOPIC_CHARS)
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_followup"));
    }
    let mut tx = state.pool.begin().await?;
    // 与待办调度统一锁序，避免会话锁和事项锁交叉等待。
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('orbit:todos:admin'))")
        .execute(&mut *tx)
        .await?;
    // 先锁来源/目标会话，再锁事项；跨会话的明确更新使用固定顺序避免死锁。
    sqlx::query(include_str!("../sql/followup_lock_conversations.sql"))
        .bind(owner)
        .bind(id)
        .bind(source.map(|s| s.job.conversation_id))
        .execute(&mut *tx)
        .await?;
    if let Some(result) = source_fence(&mut tx, source).await? {
        return Ok(result);
    }
    let canonical = crate::todos::identity::principal(state, owner).await?;
    let current_prefs: Preferences = sqlx::query_as("SELECT timezone,enabled,quiet_start,quiet_end,min_interval_minutes,version FROM followup_preferences WHERE owner=$1 FOR UPDATE").bind(&canonical).fetch_one(&mut *tx).await?;
    if current_prefs.version != prefs.version {
        return Err(ApiError(StatusCode::CONFLICT, "followup_version_conflict"));
    }
    let current: Followup = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM followups WHERE id=$1 AND personal_owner(owner)=personal_owner($2) FOR UPDATE"
    )))
    .bind(id)
    .bind(owner)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError(StatusCode::NOT_FOUND, "followup_not_found"))?;
    if current.version != input.version {
        return Err(ApiError(StatusCode::CONFLICT, "followup_version_conflict"));
    }
    if input.status == "scheduled"
        && matches!(
            current.error.as_deref(),
            Some("memory_changed" | "communication_source_changed")
        )
    {
        return Err(ApiError(StatusCode::CONFLICT, "followup_memory_changed"));
    }
    if input.status == "scheduled" && current.kind == "checkin" && !prefs.enabled {
        return Err(ApiError(StatusCode::CONFLICT, "checkins_disabled"));
    }
    let started:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM outbox WHERE followup_id=$1 AND followup_version=$2 AND dispatch_started_at IS NOT NULL)")
        .bind(id).bind(current.version).fetch_one(&mut *tx).await?;
    sqlx::query(CANCEL_OUTBOX)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(include_str!("../sql/followup_update.sql"))
        .bind(id)
        .bind(&current.owner)
        .bind(&input.status)
        .bind(&input.topic)
        .bind(due)
        .bind(&prefs.timezone)
        .execute(&mut *tx)
        .await?;
    if input.status == "scheduled" {
        sqlx::query("UPDATE todos SET status='active',closed_at=NULL,version=version+1,updated_at=now() WHERE id=(SELECT todo_id FROM todo_schedules WHERE id=(SELECT todo_schedule_id FROM followups WHERE id=$1))")
            .bind(id).execute(&mut *tx).await?;
    }
    if matches!(input.status.as_str(), "completed" | "cancelled") {
        sqlx::query("UPDATE todos SET status=$2,version=version+1,closed_at=now(),updated_at=now() WHERE id=(SELECT todo_id FROM todo_schedules WHERE id=(SELECT todo_schedule_id FROM followups WHERE id=$1))")
            .bind(id).bind(&input.status).execute(&mut *tx).await?;
        let schedules: Vec<Uuid> = sqlx::query_scalar("UPDATE todo_schedules SET status='ended',version=version+1 WHERE todo_id=(SELECT todo_id FROM todo_schedules WHERE id=(SELECT todo_schedule_id FROM followups WHERE id=$1)) RETURNING id")
            .bind(id).fetch_all(&mut *tx).await?;
        for schedule in schedules {
            crate::todos::service::cancel_pending(&mut tx, schedule).await?;
        }
    }
    let result = json!({"id":id,"version":current.version+1,"status":input.status,"due_at":due.unwrap_or(current.due_at),"timezone":prefs.timezone,"delivery_may_have_started":started});
    record(&mut tx, source, &result).await?;
    tx.commit().await?;
    Ok(result)
}
/// 原文更新/遗忘同时停止依赖该条原文或同一来源事实的任务和出站队列。
pub async fn cancel_dependencies(
    tx: &mut Transaction<'_, Postgres>,
    owner: &str,
    memory_id: Uuid,
    source_run: Option<Uuid>,
) -> ApiResult<()> {
    let ids: Vec<Uuid> =
        sqlx::query_scalar(include_str!("../sql/followup_cancel_dependencies.sql"))
            .bind(owner)
            .bind(memory_id)
            .bind(source_run)
            .fetch_all(&mut **tx)
            .await?;
    for id in ids {
        sqlx::query(CANCEL_OUTBOX)
            .bind(id)
            .execute(&mut **tx)
            .await?;
        sqlx::query("UPDATE messages SET context_visible=false WHERE followup_id=$1")
            .bind(id)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}
