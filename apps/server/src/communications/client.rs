use super::{
    configured,
    crypto::{self, Tokens},
    unavailable,
};
use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

// 外部响应有独立字节上限；请求超时短于入口的总超时。
const MAX_RESPONSE: usize = 2 * 1024 * 1024;
const REQUEST_SECONDS: u64 = 5;

/// 数据库锁定的凭证状态，与密文明文类型分离。
#[derive(sqlx::FromRow)]
struct StoredTokens {
    /// 认证加密后的令牌对。
    credentials: Vec<u8>,
    /// 访问令牌截止时间。
    expires_at: DateTime<Utc>,
    /// 刷新令牌截止时间。
    refresh_expires_at: DateTime<Utc>,
    /// 是否需要重新授权。
    status: String,
}
/// 只读取有界 JSON，并将供应商详细错误转换为稳定类别。
pub(super) async fn json_response(request: reqwest::RequestBuilder) -> ApiResult<Value> {
    let mut response = request
        .timeout(std::time::Duration::from_secs(REQUEST_SECONDS))
        .send()
        .await
        .map_err(unavailable)?;
    if !response.status().is_success() {
        return Err(unavailable("供应商拒绝"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(unavailable)? {
        if bytes.len() + chunk.len() > MAX_RESPONSE {
            return Err(unavailable("响应过大"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(unavailable)?;
    if value["error"].is_string()
        || value
            .get("code")
            .is_some_and(|code| code.as_i64() != Some(0))
    {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "communication_provider_rejected",
        ));
    }
    Ok(value)
}
/// OAuth v2 的令牌字段在顶层；不依赖旧版 tenant token 交换协议。
pub(super) async fn exchange(
    state: &AppState,
    mut body: Value,
) -> ApiResult<(Tokens, DateTime<Utc>, DateTime<Utc>)> {
    configured(state)?;
    let config = state.config.feishu.as_ref().expect("已验证配置");
    body["client_id"] = json!(config.app_id);
    body["client_secret"] = json!(config.app_secret);
    let value = json_response(
        state
            .http
            .post(format!("{}/authen/v2/oauth/token", config.api_base))
            .json(&body),
    )
    .await?;
    let access_token = string(&value, "access_token")?;
    let refresh_token = string(&value, "refresh_token")?;
    let expiry = value["expires_in"]
        .as_i64()
        .filter(|n| (60..=86400 * 365).contains(n))
        .ok_or_else(|| unavailable("缺少到期时间"))?;
    let refresh_expiry = value["refresh_token_expires_in"]
        .as_i64()
        .filter(|n| (60..=86400 * 365).contains(n))
        .ok_or_else(|| unavailable("缺少刷新期限"))?;
    Ok((
        Tokens {
            access_token,
            refresh_token,
            scopes: value["scope"]
                .as_str()
                .map(|scope| scope.split_whitespace().map(str::to_owned).collect()),
        },
        Utc::now() + Duration::seconds(expiry),
        Utc::now() + Duration::seconds(refresh_expiry),
    ))
}
/// 上游必填字符串不接受空值；不使用供应商字符串作为错误文本。
pub(super) fn string(value: &Value, key: &str) -> ApiResult<String> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() < 16384)
        .map(str::to_owned)
        .ok_or_else(|| unavailable("上游字段缺失"))
}
/// 刷新与重新授权、删除共用行锁，旋转的 refresh_token 只被消费一次。
pub(super) async fn access(state: &AppState) -> ApiResult<String> {
    configured(state)?;
    let mut tx = state.pool.begin().await?;
    let row: Option<StoredTokens> = sqlx::query_as("SELECT credentials,expires_at,refresh_expires_at,status FROM communication_connections WHERE owner='admin' FOR UPDATE").fetch_optional(&mut *tx).await?;
    let StoredTokens {
        credentials: cipher,
        expires_at: expires,
        refresh_expires_at: refresh_expires,
        status,
    } = row.ok_or(ApiError(
        StatusCode::CONFLICT,
        "communication_not_connected",
    ))?;
    if status != "active" {
        return Err(ApiError(StatusCode::CONFLICT, "communication_reauthorize"));
    }
    let tokens = crypto::open(state, &cipher)?;
    if expires > Utc::now() + Duration::minutes(2) {
        return Ok(tokens.access_token);
    }
    if refresh_expires <= Utc::now() {
        sqlx::query(
            "UPDATE communication_connections SET status='reauthorize' WHERE owner='admin'",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Err(ApiError(StatusCode::CONFLICT, "communication_reauthorize"));
    }
    let result = exchange(
        state,
        json!({"grant_type":"refresh_token","refresh_token":tokens.refresh_token}),
    )
    .await;
    let (mut refreshed, expires, refresh_expires) = match result {
        Ok(result) => result,
        Err(error) => {
            // 刷新请求可能已经消耗旧令牌；标记重连比重复消费一次性凭证更可靠。
            sqlx::query(
                "UPDATE communication_connections SET status='reauthorize' WHERE owner='admin'",
            )
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Err(error);
        }
    };
    // 刷新响应省略范围时沿用已授权范围；明确返回较小范围时取消发送能力。
    if refreshed.scopes.is_none() {
        refreshed.scopes = tokens.scopes;
    }
    let send_authorized = ["im:message", "im:message.send_as_user"]
        .iter()
        .all(|scope| {
            refreshed
                .scopes
                .as_deref()
                .unwrap_or_default()
                .iter()
                .any(|s| s == scope)
        });
    sqlx::query("UPDATE communication_connections SET credentials=$1,expires_at=$2,refresh_expires_at=$3,send_authorized=send_authorized AND $4 WHERE owner='admin'")
        .bind(crypto::seal(state,&refreshed)?).bind(expires).bind(refresh_expires).bind(send_authorized).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(refreshed.access_token)
}
/// 所有采集调用只访问固定的飞书根地址和代码指定路径。
pub(super) fn get(state: &AppState, path: &str, token: &str) -> reqwest::RequestBuilder {
    state
        .http
        .get(format!(
            "{}{}",
            state.config.feishu.as_ref().expect("已验证配置").api_base,
            path
        ))
        .bearer_auth(token)
}
