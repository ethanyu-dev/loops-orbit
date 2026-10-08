use super::{Connection, client, credentials, oauth};
use crate::{AppState, auth::Identity, error::ApiResult};
use axum::{
    Json, Router,
    extract::State,
    routing::{delete, get, post},
};
use serde_json::{Value, json};

/// 连接管理仅开放给网页管理员，OAuth 回调自行校验发起会话。
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/status", get(status))
        .route("/oauth/start", post(oauth::start))
        .route("/oauth/callback", get(oauth::callback))
        .route("/connection", delete(disconnect))
}
/// 状态投影不包含加密或明文凭证。
async fn status(State(state): State<AppState>, identity: Identity) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    let connection: Option<Connection> =
        sqlx::query_as("SELECT * FROM linear_connections WHERE owner='admin'")
            .fetch_optional(&state.pool)
            .await?;
    Ok(Json(
        json!({"configured":state.config.linear.is_some(),"connection":connection}),
    ))
}
/// 本地撤销与待回调状态清理同事务提交；已发送的更新不能被撤回。
async fn disconnect(State(state): State<AppState>, identity: Identity) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(71392311)")
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE linear_guard SET version=version+1 WHERE id=true")
        .execute(&mut *tx)
        .await?;
    let row: Option<Vec<u8>> = sqlx::query_scalar(
        "DELETE FROM linear_connections WHERE owner='admin' RETURNING credentials",
    )
    .fetch_optional(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM linear_oauth_states")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let revoked = match row.and_then(|bytes| credentials::open(&state, &bytes).ok()) {
        Some(tokens) => client::revoke(&state, tokens.refresh_token).await,
        None => false,
    };
    Ok(Json(
        json!({"disconnected":true,"provider_revoked":revoked,"inflight_updates_may_complete":true}),
    ))
}
