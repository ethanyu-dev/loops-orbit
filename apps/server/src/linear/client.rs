use super::{configured, unavailable};
use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use serde_json::{Value, json};

// 上游响应有明确大小和时间边界，不能把整个工作空间一次性塞入内存。
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const REQUEST_SECONDS: u64 = 15;
/// 接收 JSON 前检查状态并限制大小，GraphQL 的 HTTP 200 不代表操作成功。
async fn response(request: reqwest::RequestBuilder) -> ApiResult<Value> {
    let mut response = request
        .timeout(std::time::Duration::from_secs(REQUEST_SECONDS))
        .send()
        .await
        .map_err(unavailable)?;
    let status = response.status();
    match status.as_u16() {
        401 => return Err(ApiError(StatusCode::UNAUTHORIZED, "linear_invalid_key")),
        403 => return Err(ApiError(StatusCode::FORBIDDEN, "linear_permission_denied")),
        429 => {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "linear_rate_limited",
            ));
        }
        _ if status.is_server_error() => return Err(unavailable("上游失败")),
        _ => {}
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(unavailable)? {
        if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(unavailable("响应过大"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(unavailable)?;
    // Linear 的 GraphQL 限流使用 HTTP 400，认证和权限错误也可能放在 errors 中。
    if let Some(errors) = value["errors"]
        .as_array()
        .filter(|errors| !errors.is_empty())
    {
        // 有部分数据时写操作可能已经生效，不能把字段级拒绝误报为整个请求未执行。
        if !value["data"].is_null() {
            return Err(ApiError(StatusCode::BAD_GATEWAY, "linear_graphql_error"));
        }
        if errors
            .iter()
            .any(|error| error["extensions"]["code"] == "RATELIMITED")
        {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "linear_rate_limited",
            ));
        }
        if errors.iter().any(|error| {
            matches!(
                error["extensions"]["code"].as_str(),
                Some("AUTHENTICATION_ERROR" | "UNAUTHENTICATED")
            )
        }) {
            return Err(ApiError(StatusCode::UNAUTHORIZED, "linear_invalid_key"));
        }
        if errors
            .iter()
            .any(|error| matches!(error["extensions"]["code"].as_str(), Some("FORBIDDEN")))
        {
            return Err(ApiError(StatusCode::FORBIDDEN, "linear_permission_denied"));
        }
        return Err(ApiError(StatusCode::BAD_GATEWAY, "linear_graphql_error"));
    }
    if !status.is_success() {
        return Err(unavailable("上游失败"));
    }
    Ok(value)
}
/// 只允许服务端固定 GraphQL 文档；部分 errors 也不能当作成功。
pub(super) async fn graphql(
    state: &AppState,
    token: &str,
    query: &str,
    variables: Value,
) -> ApiResult<Value> {
    let value = response(
        state
            .http
            .post(format!("{}/graphql", configured(state)?.api_base))
            .header(reqwest::header::AUTHORIZATION, token)
            .json(&json!({"query":query,"variables":variables})),
    )
    .await;
    // 明确失效的密钥不继续出现在工具目录中；权限不足不等于整个账号凭据失效。
    if value
        .as_ref()
        .is_err_and(|error| error.1 == "linear_invalid_key")
    {
        sqlx::query("UPDATE linear_connections SET status='reauthorize' WHERE key_fingerprint=$1")
            .bind(crate::auth::hash(token))
            .execute(&state.pool)
            .await?;
    }
    let value = value?;
    value
        .get("data")
        .filter(|d| d.is_object())
        .cloned()
        .ok_or_else(|| unavailable("缺少数据"))
}
