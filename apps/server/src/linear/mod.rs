mod client;
mod connection;
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

/// 个人密钥只由服务端环境注入，不实现 Debug，避免凭据进入日志。
#[derive(Clone)]
pub struct Config {
    /// Linear 个人 API Key，权限和可访问团队由供应商控制。
    pub api_key: String,
    /// 官方 API 根地址；仅测试代码可替换为本地协议夹具。
    pub api_base: String,
    /// 可选固定工作空间，防止误连其他工作空间。
    pub workspace_slug: Option<String>,
}
impl Config {
    /// 未配置个人密钥时关闭能力，不再依赖 OAuth 应用或令牌加密配置。
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        Self::parse(
            std::env::var("LINEAR_API_KEY").unwrap_or_default(),
            std::env::var("LINEAR_WORKSPACE_SLUG").ok(),
        )
    }
    /// 校验密钥能作为单个请求头值使用，错误信息不携带输入。
    fn parse(api_key: String, workspace_slug: Option<String>) -> anyhow::Result<Option<Self>> {
        let api_key = api_key.trim().to_owned();
        if api_key.is_empty() {
            return Ok(None);
        }
        anyhow::ensure!(
            api_key.bytes().all(|b| b.is_ascii_graphic()),
            "LINEAR_API_KEY 格式错误"
        );
        Ok(Some(Self {
            api_key,
            api_base: "https://api.linear.app".into(),
            workspace_slug: workspace_slug
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty()),
        }))
    }
    /// 数据库只保存不可逆摘要；更换密钥后旧连接不能继续使用新凭据。
    fn fingerprint(&self) -> String {
        crate::auth::hash(&self.api_key)
    }
}
/// 已验证的账号元数据；权限由 Linear 按请求检查，不伪造 OAuth scopes。
#[derive(Clone, Serialize, sqlx::FromRow)]
pub(super) struct Connection {
    /// 每次连接产生新代次，旧任务不能继承新连接。
    generation: Uuid,
    /// 身份查询返回的实际用户，是工具中 me 的唯一来源。
    user_id: String,
    /// 可读账号名称。
    user_name: String,
    /// 实际工作空间 ID。
    workspace_id: String,
    /// 工作空间展示名。
    workspace_name: String,
    /// 用于对照用户提供的 Linear 地址。
    workspace_slug: String,
    /// 只用于匹配服务端当前密钥，不向浏览器或模型返回。
    #[serde(skip_serializing)]
    key_fingerprint: Option<String>,
    /// active 表示已验证身份；reauthorize 表示密钥失效，需要重新配置并连接。
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
/// 只返回稳定分类，不泄露上游错误正文或请求凭证。
fn unavailable(_: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::BAD_GATEWAY, "linear_unavailable")
}
/// 参数不合法时在网络请求前失败。
fn invalid() -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, "invalid_arguments")
}
/// 个人密钥仅供 Orbit 网页所有者使用，不要求此人在 Linear 中具有管理员角色。
async fn connection(state: &AppState, owner: &str) -> ApiResult<Option<Connection>> {
    if owner != "admin" {
        return Ok(None);
    }
    Ok(connection::current(state)
        .await?
        .filter(|row| row.status == "active"))
}

/// 只读定时任务记录连接代次和密钥摘要，断开或换号后旧报告不能继续投递。
pub(crate) async fn todo_revision(state: &AppState) -> ApiResult<Option<String>> {
    Ok(connection(state, "admin").await?.map(|c| {
        crate::auth::hash(&format!(
            "{}:{}",
            c.generation,
            c.key_fingerprint.unwrap_or_default()
        ))
    }))
}

/// 待办仅暴露查询白名单，后台身份来自已验证的个人安排，不能执行写操作。
pub(crate) async fn read_for_todo(
    state: &AppState,
    name: &str,
    args: serde_json::Value,
) -> ApiResult<serde_json::Value> {
    if !matches!(
        name,
        "linear_issue_list" | "linear_issue_get" | "linear_team_metadata"
    ) {
        return Err(ApiError(StatusCode::FORBIDDEN, "todo_read_only"));
    }
    let connection = connection(state, "admin")
        .await?
        .ok_or(ApiError(StatusCode::CONFLICT, "linear_not_connected"))?;
    let token = &configured(state)?.api_key;
    let result = queries::execute(state, &connection, token, name, args).await?;
    connection::access(state, connection.generation).await?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 验证纯配置解析，不修改进程环境；不验证真实密钥或供应商权限。
    #[test]
    fn personal_key_configuration() {
        assert!(Config::parse("  ".into(), None).unwrap().is_none());
        let config = Config::parse(" fixture-key ".into(), Some(" pplabs ".into()))
            .unwrap()
            .unwrap();
        assert_eq!(config.api_key, "fixture-key");
        assert_eq!(config.workspace_slug.as_deref(), Some("pplabs"));
        assert_ne!(config.fingerprint(), config.api_key);
        assert!(Config::parse("key\r\nInjected: value".into(), None).is_err());
        assert!(Config::parse("key with space".into(), None).is_err());
    }
}
