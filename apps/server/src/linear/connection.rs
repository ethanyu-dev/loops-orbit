use super::{Connection, client, configured, unavailable};
use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

/// 展示和使用连接前都匹配当前密钥及工作空间；旧 OAuth 行不会被视为个人密钥连接。
pub(super) async fn current(state: &AppState) -> ApiResult<Option<Connection>> {
    let Some(config) = &state.config.linear else {
        return Ok(None);
    };
    let row: Option<Connection> = sqlx::query_as(
        "SELECT * FROM linear_connections WHERE owner='admin' AND key_fingerprint=$1",
    )
    .bind(config.fingerprint())
    .fetch_optional(&state.pool)
    .await?;
    Ok(row.filter(|row| {
        config
            .workspace_slug
            .as_ref()
            .is_none_or(|slug| slug == &row.workspace_slug)
    }))
}

/// 只读取身份进行验证，不通过试写探测权限；断开或另一连接完成后，旧验证结果失效。
pub(super) async fn connect(state: &AppState) -> ApiResult<()> {
    let config = configured(state)?;
    let version: i64 = sqlx::query_scalar("SELECT version FROM linear_guard WHERE id=true")
        .fetch_one(&state.pool)
        .await?;
    let data = client::graphql(
        state,
        &config.api_key,
        include_str!("../../graphql/linear_identity.graphql"),
        json!({}),
    )
    .await?;
    let field = |parent: &str, key: &str| {
        data[parent][key]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| unavailable("身份字段缺失"))
    };
    let slug = field("organization", "urlKey")?;
    if config
        .workspace_slug
        .as_ref()
        .is_some_and(|expected| expected != &slug)
    {
        return Err(ApiError(StatusCode::CONFLICT, "linear_workspace_mismatch"));
    }
    let mut tx = state.pool.begin().await?;
    let current: i64 =
        sqlx::query_scalar("SELECT version FROM linear_guard WHERE id=true FOR UPDATE")
            .fetch_one(&mut *tx)
            .await?;
    if current != version {
        return Err(ApiError(StatusCode::CONFLICT, "linear_connection_changed"));
    }
    sqlx::query(include_str!("../sql/linear_connection_save.sql"))
        .bind(Uuid::new_v4())
        .bind(field("viewer", "id")?)
        .bind(field("viewer", "name")?)
        .bind(field("organization", "id")?)
        .bind(field("organization", "name")?)
        .bind(slug)
        .bind(config.fingerprint())
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE linear_guard SET version=version+1 WHERE id=true")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// 执行前重查代次和密钥，避免旧工具实例借用重新连接后的账号。
pub(super) async fn access(state: &AppState, generation: Uuid) -> ApiResult<Connection> {
    super::connection(state, "admin")
        .await?
        .filter(|row| row.generation == generation)
        .ok_or(ApiError(StatusCode::CONFLICT, "linear_connection_changed"))
}
