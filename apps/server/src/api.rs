use crate::{
    AppState,
    auth::{self, Identity},
    error::{ApiError, ApiResult},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use uuid::Uuid;

// 输入按 Unicode 字符计数，防止一条消息耗尽上下文；临时授权最长三十天。
pub const MAX_INPUT_CHARS: usize = 12_000;
// 同批补充保持在原文预算内，取消前不会提前写入历史摘要。
const MAX_BATCH_MESSAGES: i64 = 30;
const MAX_BATCH_CHARS: i64 = 24_000;
const MAX_LINK_SECONDS: i64 = 30 * 24 * 3600;

/// 浏览器可见的会话摘要，不携带认证数据。
#[derive(Serialize, FromRow)]
pub struct Conversation {
    /// 会话的稳定标识。
    id: Uuid,
    /// 第一条消息生成的短标题。
    title: String,
    /// 用于在界面区分网页和飞书入口。
    channel: String,
    /// 最近一次提交的时间。
    updated_at: DateTime<Utc>,
}

/// 单条可见消息的响应模型，直接映射查询字段，避免位置元组转 JSON。
#[derive(Serialize, FromRow)]
pub struct MessageView {
    /// 消息在数据库中的稳定序号。
    id: i64,
    /// 由服务端限定的 user 或 assistant 角色。
    role: String,
    /// 文本正文。
    content: String,
    /// 关联运行，用于显示失败或排队状态。
    run_id: Option<Uuid>,
    /// 主动通知有独立来源，不伪造用户轮次。
    kind: String,
    /// 可关联提醒事项。
    followup_id: Option<Uuid>,
}

/// 对话中的任务摘要，不暴露租约和内部输入副本。
#[derive(Serialize, FromRow)]
pub struct RunView {
    /// 客户端跟踪的任务标识。
    id: Uuid,
    /// 持久化运行状态。
    status: String,
    /// 可显示的稳定错误分类。
    error: Option<String>,
    /// 尚未完成的回复快照，失败或取消后不当作正式答案。
    partial_content: String,
    /// 当前处于上下文整理还是回复生成阶段。
    phase: String,
}

/// 会话详情保持已有 JSON 协议，编译器检查字段映射。
#[derive(Serialize)]
pub struct ConversationDetail {
    /// 按轮次正序排列的可见消息。
    messages: Vec<MessageView>,
    /// 最近任务的状态摘要。
    runs: Vec<RunView>,
}

/// 列出当前身份可见的最近会话；管理员也能查看飞书和访客会话。
pub async fn conversations(
    State(state): State<AppState>,
    identity: Identity,
) -> ApiResult<Json<Vec<Conversation>>> {
    let conversations = sqlx::query_as(
        r#"
        SELECT id, title, channel, updated_at
        FROM conversations
        WHERE owner = $1 OR $2
        ORDER BY updated_at DESC
        LIMIT 100
    "#,
    )
    .bind(&identity.owner)
    .bind(identity.admin)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(conversations))
}

/// 显式创建空会话，后续消息重试复用同一个会话 ID。
pub async fn create_conversation(
    State(state): State<AppState>,
    identity: Identity,
) -> ApiResult<Json<Value>> {
    auth::limit(&state, &format!("create:{}", identity.owner), 20).await?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO conversations(id,owner,channel,title) VALUES($1,$2,'web','新对话')")
        .bind(id)
        .bind(identity.owner)
        .execute(&state.pool)
        .await?;
    Ok(Json(json!({"id":id})))
}

/// 获取消息和任务状态；刷新浏览器后仍能跟踪已入队的请求。
pub async fn detail(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ConversationDetail>> {
    authorize(&state, &identity, id).await?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let mut messages: Vec<MessageView> = sqlx::query_as(
        r#"
        SELECT m.seq AS id, m.role, m.content, m.run_id, m.kind, m.followup_id
        FROM messages m
        WHERE m.conversation_id = $1
        ORDER BY m.seq DESC
        LIMIT 200
    "#,
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let runs: Vec<RunView> = sqlx::query_as(
        "SELECT id,status,error,partial_content,phase FROM runs WHERE conversation_id=$1 ORDER BY seq DESC LIMIT 200",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    messages.reverse();
    Ok(Json(ConversationDetail { messages, runs }))
}

/// 由浏览器生成幂等键，网络重试不应产生第二次模型调用。
#[derive(Deserialize)]
pub struct SendMessage {
    /// 用户输入文本。
    pub content: String,
    /// 每次用户发送动作一个 UUID。
    pub idempotency_key: Uuid,
}

/// 事务中同时写入输入与任务，客户端连接断开不会取消已接受的任务。
pub async fn send(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
    Json(body): Json<SendMessage>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &identity, id).await?;
    auth::limit(&state, &format!("send:{}", identity.owner), 30).await?;
    let content = body.content.trim();
    validate_input(content)?;
    let mut tx = state.pool.begin().await?;
    // 会话行锁让容量检查、幂等判断和输入写入按顺序提交。
    sqlx::query("SELECT id FROM conversations WHERE id=$1 FOR UPDATE")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let existing: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id,input FROM runs WHERE conversation_id=$1 AND idempotency_key=$2")
            .bind(id)
            .bind(body.idempotency_key.to_string())
            .fetch_optional(&mut *tx)
            .await?;
    if let Some((run_id, input)) = existing {
        if input != content {
            return Err(ApiError(StatusCode::CONFLICT, "idempotency_conflict"));
        }
        return Ok(Json(json!({"run_id":run_id})));
    }
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM runs WHERE conversation_id=$1 AND status IN ('queued','running')",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if pending >= 5 {
        return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "conversation_busy"));
    }
    let run_id = enqueue(
        &mut tx,
        id,
        &body.idempotency_key.to_string(),
        content,
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"run_id":run_id})))
}

/// 统一输入边界，网页与飞书复用。
pub fn validate_input(content: &str) -> ApiResult<()> {
    if content.is_empty() || content.chars().count() > MAX_INPUT_CHARS {
        Err(ApiError(StatusCode::BAD_REQUEST, "invalid_message_length"))
    } else {
        Ok(())
    }
}

/// 在调用者事务中创建任务和输入，确保事件去重与入队可以一起提交。
pub async fn enqueue(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    conversation: Uuid,
    key: &str,
    content: &str,
    reply_to: Option<&str>,
) -> ApiResult<Uuid> {
    let run_id = Uuid::new_v4();
    let batch: Option<Uuid> = sqlx::query_scalar(
        "SELECT batch_id FROM runs WHERE conversation_id=$1 AND status IN ('queued','running') ORDER BY seq LIMIT 1",
    ).bind(conversation).fetch_optional(&mut **tx).await?;
    if let Some(batch) = batch {
        let (count, chars): (i64, i64) = sqlx::query_as(
            "SELECT count(*),COALESCE(sum(char_length(input)),0)::bigint FROM runs WHERE conversation_id=$1 AND batch_id=$2",
        ).bind(conversation).bind(batch).fetch_one(&mut **tx).await?;
        if count >= MAX_BATCH_MESSAGES || chars + content.chars().count() as i64 > MAX_BATCH_CHARS {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "pending_context_full",
            ));
        }
    }
    sqlx::query(
        "UPDATE runs SET status='superseded', partial_content='', lease_token=NULL, lease_until=NULL, finished_at=now() WHERE conversation_id=$1 AND status IN ('queued','running')",
    ).bind(conversation).execute(&mut **tx).await?;
    sqlx::query(
        r#"
        INSERT INTO runs (id, conversation_id, idempotency_key, input, reply_to, batch_id, available_at)
        VALUES ($1, $2, $3, $4, $5, $6, now() + interval '600 milliseconds')
    "#,
    )
    .bind(run_id)
    .bind(conversation)
    .bind(key)
    .bind(content)
    .bind(reply_to)
    .bind(batch.unwrap_or(run_id))
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO messages(conversation_id,run_id,role,content) VALUES($1,$2,'user',$3)",
    )
    .bind(conversation)
    .bind(run_id)
    .bind(content)
    .execute(&mut **tx)
    .await?;
    let title: String = content.chars().take(36).collect();
    sqlx::query(
        r#"
        UPDATE conversations
        SET title = CASE WHEN title = '新对话' THEN $2 ELSE title END,
            updated_at = now()
        WHERE id = $1
    "#,
    )
    .bind(conversation)
    .bind(title)
    .execute(&mut **tx)
    .await?;
    Ok(run_id)
}

/// 只取消客户端看到的那次执行；晚到的停止请求不会取消随后提交的新消息。
pub async fn cancel(
    State(state): State<AppState>,
    identity: Identity,
    Path((id, run_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &identity, id).await?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT id FROM conversations WHERE id=$1 FOR UPDATE")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let batch: Option<Uuid> = sqlx::query_scalar(
        "SELECT batch_id FROM runs WHERE conversation_id=$1 AND id=$2 AND status IN ('queued','running')",
    ).bind(id).bind(run_id).fetch_optional(&mut *tx).await?;
    if let Some(batch) = batch {
        sqlx::query(
            "UPDATE runs SET status='cancelled', partial_content='', lease_token=NULL, lease_until=NULL, finished_at=now() WHERE conversation_id=$1 AND batch_id=$2 AND status IN ('queued','running','superseded')",
        ).bind(id).bind(batch).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(Json(json!({"cancelled":batch.is_some()})))
}

/// 未授权与不存在统一返回 404，避免暴露其他人的会话 ID。
async fn authorize(state: &AppState, identity: &Identity, id: Uuid) -> ApiResult<()> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM conversations WHERE id=$1 AND (owner=$2 OR $3))",
    )
    .bind(id)
    .bind(&identity.owner)
    .bind(identity.admin)
    .fetch_one(&state.pool)
    .await?;
    if exists {
        Ok(())
    } else {
        Err(ApiError(StatusCode::NOT_FOUND, "conversation_not_found"))
    }
}

/// 临时链接创建参数，权限固定为访客聊天。
#[derive(Deserialize)]
pub struct CreateLink {
    /// 显示在管理台的用途标签。
    label: String,
    /// 从创建时起的有效秒数。
    expires_in_seconds: i64,
}

/// 原始链接只在创建响应中返回一次，后续列表只显示元数据。
pub async fn create_link(
    State(state): State<AppState>,
    identity: Identity,
    Json(body): Json<CreateLink>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    if !(60..=MAX_LINK_SECONDS).contains(&body.expires_in_seconds)
        || body.label.trim().is_empty()
        || body.label.chars().count() > 80
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_link_options"));
    }
    let id = Uuid::new_v4();
    let token = auth::new_token();
    let expires_at = Utc::now() + chrono::Duration::seconds(body.expires_in_seconds);
    sqlx::query("INSERT INTO grants(id,token_hash,label,expires_at) VALUES($1,$2,$3,$4)")
        .bind(id)
        .bind(auth::hash(&token))
        .bind(body.label.trim())
        .bind(expires_at)
        .execute(&state.pool)
        .await?;
    tracing::info!(grant_id=%id,"创建临时授权");
    Ok(Json(json!({
        "id": id,
        "url": format!("{}/#token={token}", state.config.public_url),
        "expires_at": expires_at,
    })))
}

/// 管理台使用的授权元数据，刻意不包含 token 或摘要。
#[derive(Serialize, FromRow)]
pub struct GrantView {
    /// 撤销操作的稳定标识。
    id: Uuid,
    /// 管理员提供的用途标签。
    label: String,
    /// 授权截止时间。
    expires_at: DateTime<Utc>,
    /// 已撤销时记录操作时间。
    revoked_at: Option<DateTime<Utc>>,
    /// 创建时间用于最近优先排序。
    created_at: DateTime<Utc>,
}

/// 列出授权有效期和撤销状态，绝不返回 token 摘要。
pub async fn links(
    State(state): State<AppState>,
    identity: Identity,
) -> ApiResult<Json<Vec<GrantView>>> {
    identity.require_admin()?;
    let grants = sqlx::query_as(
        r#"
        SELECT id, label, expires_at, revoked_at, created_at
        FROM grants
        ORDER BY created_at DESC
        LIMIT 100
    "#,
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(grants))
}

/// 撤销立即影响所有关联 Cookie，不依赖后台清理。
pub async fn revoke_link(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    let result = sqlx::query("UPDATE grants SET revoked_at=COALESCE(revoked_at,now()) WHERE id=$1")
        .bind(id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError(StatusCode::NOT_FOUND, "link_not_found"));
    }
    tracing::info!(grant_id=%id,"撤销临时授权");
    Ok(Json(json!({"ok":true})))
}

/// 管理台展示跨实例共享的队列状态和当前运行配置。
pub async fn status(State(state): State<AppState>, identity: Identity) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    Ok(Json(snapshot(&state).await?))
}

/// Prometheus 与管理台使用相同的数据库口径，不将实例内计数误当作全局计数。
pub async fn snapshot(state: &AppState) -> ApiResult<Value> {
    let runs: Vec<(String, i64)> =
        sqlx::query_as("SELECT status,count(*) FROM runs GROUP BY status")
            .fetch_all(&state.pool)
            .await?;
    let outbox: Vec<(String, i64)> =
        sqlx::query_as("SELECT status,count(*) FROM outbox GROUP BY status")
            .fetch_all(&state.pool)
            .await?;
    let mut counts =
        json!({"queued":0,"running":0,"completed":0,"failed":0,"cancelled":0,"superseded":0});
    for (key, count) in runs {
        counts[key] = json!(count);
    }
    let mut delivery = json!({"queued":0,"running":0,"completed":0,"failed":0});
    for (key, count) in outbox {
        delivery[key] = json!(count);
    }
    Ok(json!({
        "runs": counts,
        "delivery": delivery,
        "model": state.config.model.model,
        "feishu_enabled": state.config.feishu.is_some(),
        "workers_per_instance": state.config.workers,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}
