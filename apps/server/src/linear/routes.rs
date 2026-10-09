use super::connection;
use crate::{AppState, auth::Identity, error::ApiResult};
use axum::{Json, Router, extract::State, routing::get};
use serde_json::{Value, json};

/// 连接管理仅开放给 Orbit 网页所有者，和 Linear 工作空间管理员角色无关。
pub fn router() -> Router<AppState> {
    Router::new().route("/status", get(status)).route(
        "/connection",
        axum::routing::post(connect).delete(disconnect),
    )
}
/// 状态投影不包含密钥、摘要或未经验证的权限声明。
async fn status(State(state): State<AppState>, identity: Identity) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    Ok(Json(
        json!({"configured":state.config.linear.is_some(),"connection":connection::current(&state).await?}),
    ))
}
/// 密钥只从服务端配置读取，网页仅触发身份验证，不接受浏览器传入凭据。
async fn connect(State(state): State<AppState>, identity: Identity) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    connection::connect(&state).await?;
    Ok(Json(json!({"connected":true})))
}
/// 本地断开不会吊销供应商密钥，已发送的更新也不能撤回。
async fn disconnect(State(state): State<AppState>, identity: Identity) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("UPDATE linear_guard SET version=version+1 WHERE id=true")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM linear_connections WHERE owner='admin'")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(
        json!({"disconnected":true,"inflight_updates_may_complete":true}),
    ))
}
