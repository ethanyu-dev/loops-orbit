mod client;
mod credentials;
mod oauth;
mod queries;
pub mod routes;
pub(crate) mod tools;
mod update;

use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use serde::Serialize;
use uuid::Uuid;

// OAuth 回调只使用配置的 API 源，避免模型或请求参数控制凭证发送地址。
const CALLBACK: &str = "/api/linear/oauth/callback";
/// 启用 Linear 所需的独立应用配置，不实现 Debug 以防凭证进入日志。
#[derive(Clone)]
pub struct Config {
    /// 用户 OAuth 应用 ID。
    pub client_id: String,
    /// 仅用于服务端令牌交换的密钥。
    pub client_secret: String,
    /// 独立的令牌认证加密密钥。
    pub token_key: [u8; 32],
    /// 官方 API 根地址；仅测试代码可替换为本地协议夹具。
    pub api_base: String,
    /// 可选固定工作空间，部署者可限定为 pplabs。
    pub workspace_slug: Option<String>,
}
impl Config {
    /// 缺少应用 ID 时关闭能力；部分配置不允许静默启用。
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let id = std::env::var("LINEAR_CLIENT_ID").unwrap_or_default();
        if id.trim().is_empty() {
            return Ok(None);
        }
        let secret = std::env::var("LINEAR_CLIENT_SECRET").unwrap_or_default();
        anyhow::ensure!(!secret.trim().is_empty(), "缺少 LINEAR_CLIENT_SECRET");
        let key = hex::decode(std::env::var("LINEAR_TOKEN_KEY").unwrap_or_default())
            .map_err(|_| anyhow::anyhow!("LINEAR_TOKEN_KEY 必须为 64 位十六进制"))?;
        Ok(Some(Self {
            client_id: id,
            client_secret: secret,
            token_key: key
                .try_into()
                .map_err(|_| anyhow::anyhow!("LINEAR_TOKEN_KEY 必须为 64 位十六进制"))?,
            api_base: "https://api.linear.app".into(),
            workspace_slug: std::env::var("LINEAR_WORKSPACE_SLUG")
                .ok()
                .filter(|s| !s.trim().is_empty()),
        }))
    }
}
/// 身份与授权元数据允许展示；密文、令牌和刷新信息不序列化。
#[derive(Clone, Serialize, sqlx::FromRow)]
pub(super) struct Connection {
    /// 重连产生新代次，旧任务不能使用新凭证。
    generation: Uuid,
    /// 实际 OAuth 用户，是工具中 me 的唯一来源。
    user_id: String,
    /// 可读账号名称。
    user_name: String,
    /// 实际授权工作空间 ID。
    workspace_id: String,
    /// 工作空间展示名。
    workspace_name: String,
    /// 用于对照用户提供的 Linear 地址。
    workspace_slug: String,
    /// 仅令牌刷新层读取的密文。
    #[serde(skip_serializing)]
    credentials: Vec<u8>,
    /// 令牌到期时由行锁串行刷新。
    #[serde(skip_serializing)]
    expires_at: chrono::DateTime<chrono::Utc>,
    /// 实际供应商授予的 scopes。
    scopes: Vec<String>,
    /// active 或需要重新授权。
    status: String,
}
/// 缺少配置不能调用任何 Linear 端点。
fn configured(state: &AppState) -> ApiResult<&Config> {
    state
        .config
        .linear
        .as_ref()
        .ok_or(ApiError(StatusCode::CONFLICT, "linear_disabled"))
}
/// 仅返回稳定分类，不泄露上游错误正文或请求凭证。
fn unavailable(_: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::BAD_GATEWAY, "linear_unavailable")
}
/// 参数不合法时在网络请求前失败。
fn invalid() -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, "invalid_arguments")
}
/// 初始只支持管理员独立授权，不默认合并网页、访客和飞书账号。
async fn connection(state: &AppState, owner: &str) -> ApiResult<Option<Connection>> {
    if owner != "admin" || state.config.linear.is_none() {
        return Ok(None);
    }
    Ok(
        sqlx::query_as("SELECT * FROM linear_connections WHERE owner=$1 AND status='active'")
            .bind(owner)
            .fetch_optional(&state.pool)
            .await?,
    )
}
