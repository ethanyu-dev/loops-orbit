use super::{COLUMNS, Create, Followup, Preferences, Update};
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
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

/// 当前身份自己的事项与偏好；管理员查看他人会话不会获得跨身份提醒权限。
pub async fn index(State(state): State<AppState>, identity: Identity) -> ApiResult<Json<Value>> {
    let preferences = super::preferences(&state, &identity.owner).await?;
    let items: Vec<Followup> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM followups WHERE owner=$1 ORDER BY (status IN('scheduled','checking','queued','sent')) DESC,updated_at DESC LIMIT 200"
    )))
    .bind(&identity.owner)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(json!({"preferences":preferences,"items":items})))
}
/// 明确创建无需二次确认，服务端验证时间、接收身份与配额。
pub async fn create(
    State(state): State<AppState>,
    identity: Identity,
    Json(input): Json<Create>,
) -> ApiResult<Json<Value>> {
    auth::limit(&state, &format!("followups:{}", identity.owner), 30).await?;
    Ok(Json(
        super::create(&state, &identity.owner, &input, None).await?,
    ))
}
/// 改期/取消/完成都使用版本校验，旧页面无法覆盖新动作。
pub async fn update(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
    Json(input): Json<Update>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        super::update(&state, &identity.owner, id, &input, None).await?,
    ))
}
/// 偏好只控制本人回访；关闭立即使未发出的回访失效。
pub async fn prefs(
    State(state): State<AppState>,
    identity: Identity,
    Json(input): Json<Preferences>,
) -> ApiResult<Json<Value>> {
    super::save_preferences(&state, &identity.owner, &input).await?;
    Ok(Json(json!(
        super::preferences(&state, &identity.owner).await?
    )))
}
/// 通知来自真实已投递的助手消息，不把排队中内容当作已经联系用户。
#[derive(Serialize, sqlx::FromRow)]
struct Notification {
    /// 用于单条已读确认，避免读掉刚到达的其他通知。
    id: i64,
    /// 打开后可接着对话。
    conversation_id: Uuid,
    /// 对应待办，可改期或结束。
    followup_id: Uuid,
    /// 已实际发出的正文。
    content: String,
    /// 实际写入会话时间。
    created_at: chrono::DateTime<chrono::Utc>,
    /// 已读只影响网页提示，不代表用户已完成事项。
    read_at: Option<chrono::DateTime<chrono::Utc>>,
}
/// 网页轮询未读数量，网页关闭后仍保留通知；首版不提供操作系统推送。
pub async fn notifications(
    State(state): State<AppState>,
    identity: Identity,
) -> ApiResult<Json<Value>> {
    let items:Vec<Notification>=sqlx::query_as("SELECT m.seq AS id,m.conversation_id,m.followup_id,m.content,m.created_at,m.read_at FROM messages m JOIN conversations c ON c.id=m.conversation_id WHERE c.owner=$1 AND m.kind='followup' ORDER BY (m.read_at IS NULL) DESC,m.seq DESC LIMIT 50")
        .bind(&identity.owner).fetch_all(&state.pool).await?;
    let unread:i64=sqlx::query_scalar("SELECT count(*) FROM messages m JOIN conversations c ON c.id=m.conversation_id WHERE c.owner=$1 AND m.kind='followup' AND m.read_at IS NULL")
        .bind(&identity.owner).fetch_one(&state.pool).await?;
    Ok(Json(json!({"unread":unread,"items":items})))
}
/// 按消息 ID 幂等标为已读，不能操作其他身份通知。
pub async fn read(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let rows=sqlx::query("UPDATE messages SET read_at=COALESCE(read_at,now()) WHERE seq=$1 AND kind='followup' AND conversation_id IN(SELECT id FROM conversations WHERE owner=$2)")
        .bind(id).bind(&identity.owner).execute(&state.pool).await?;
    if rows.rows_affected() == 0 {
        return Err(ApiError(StatusCode::NOT_FOUND, "notification_not_found"));
    }
    Ok(Json(json!({"ok":true})))
}
