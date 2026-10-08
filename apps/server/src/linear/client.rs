use super::{
    Connection, configured,
    credentials::{self, Tokens},
    unavailable,
};
use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

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
        401 | 403 => return Err(ApiError(StatusCode::UNAUTHORIZED, "linear_reauthorize")),
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
    // Linear 的 GraphQL 限流使用 HTTP 400；OAuth 刷新失效也通过结构化错误区分。
    if value["error"] == "invalid_grant" {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "linear_reauthorize"));
    }
    if let Some(errors) = value["errors"]
        .as_array()
        .filter(|errors| !errors.is_empty())
    {
        if errors
            .iter()
            .any(|error| error["extensions"]["code"] == "RATELIMITED")
        {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "linear_rate_limited",
            ));
        }
        return Err(ApiError(StatusCode::BAD_GATEWAY, "linear_graphql_error"));
    }
    if !status.is_success() {
        return Err(unavailable("上游失败"));
    }
    Ok(value)
}
/// OAuth 只向固定端点发送表单；参数来自服务端流程而不是工具输入。
pub(super) async fn exchange(
    state: &AppState,
    fields: &[(&str, String)],
) -> ApiResult<(Tokens, chrono::DateTime<chrono::Utc>, Option<Vec<String>>)> {
    let config = configured(state)?;
    let mut params = fields.to_vec();
    params.push(("client_id", config.client_id.clone()));
    params.push(("client_secret", config.client_secret.clone()));
    // reqwest 未开启 form 特性，使用 URL 编码生成标准 application/x-www-form-urlencoded 表单。
    let body = reqwest::Url::parse_with_params("https://unused.invalid", &params)
        .map_err(unavailable)?
        .query()
        .unwrap_or_default()
        .to_owned();
    let value = response(
        state
            .http
            .post(format!("{}/oauth/token", config.api_base))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body),
    )
    .await?;
    let string = |key| {
        value[key]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| unavailable("令牌字段缺失"))
    };
    let seconds = value["expires_in"]
        .as_i64()
        .filter(|s| *s > 0 && *s <= 366 * 86400)
        .ok_or_else(|| unavailable("令牌期限错误"))?;
    let scopes = match &value["scope"] {
        Value::String(s) => Some(s.split_whitespace().map(str::to_owned).collect()),
        Value::Array(values) => Some(
            values
                .iter()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect(),
        ),
        _ => None,
    };
    Ok((
        Tokens {
            access_token: string("access_token")?,
            refresh_token: string("refresh_token")?,
        },
        chrono::Utc::now() + chrono::Duration::seconds(seconds),
        scopes,
    ))
}
/// 数据库行锁串行刷新；刷新结果先提交，后续业务失败不能回滚新令牌。
pub(super) async fn access(state: &AppState, generation: Uuid) -> ApiResult<(Connection, Tokens)> {
    configured(state)?;
    let mut tx = state.pool.begin().await?;
    let mut row:Connection=sqlx::query_as("SELECT * FROM linear_connections WHERE owner='admin' AND generation=$1 AND status='active' FOR UPDATE").bind(generation).fetch_optional(&mut *tx).await?.ok_or(ApiError(StatusCode::FORBIDDEN,"linear_connection_changed"))?;
    let mut tokens = credentials::open(state, &row.credentials)?;
    if row.expires_at < chrono::Utc::now() + chrono::Duration::seconds(60) {
        let refreshed = exchange(
            state,
            &[
                ("grant_type", "refresh_token".into()),
                ("refresh_token", tokens.refresh_token.clone()),
            ],
        )
        .await;
        let (new, expires, scopes) = match refreshed {
            Ok(value) => value,
            Err(error) => {
                if error.1 == "linear_reauthorize" {
                    sqlx::query(
                        "UPDATE linear_connections SET status='reauthorize' WHERE owner='admin'",
                    )
                    .execute(&mut *tx)
                    .await?;
                    tx.commit().await?;
                }
                return Err(error);
            }
        };
        row.credentials = credentials::seal(state, &new)?;
        row.expires_at = expires;
        if let Some(scopes) = scopes {
            row.scopes = scopes;
        }
        sqlx::query("UPDATE linear_connections SET credentials=$1,expires_at=$2,scopes=$3 WHERE owner='admin' AND generation=$4").bind(&row.credentials).bind(expires).bind(&row.scopes).bind(generation).execute(&mut *tx).await?;
        tokens = new;
    }
    tx.commit().await?;
    Ok((row, tokens))
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
            .bearer_auth(token)
            .json(&json!({"query":query,"variables":variables})),
    )
    .await?;
    value
        .get("data")
        .filter(|d| d.is_object())
        .cloned()
        .ok_or_else(|| unavailable("缺少数据"))
}
/// 断开先撤销本地访问；供应商撤销失败时保留明确反馈，不恢复本地连接。
pub(super) async fn revoke(state: &AppState, token: String) -> bool {
    let Ok(config) = configured(state) else {
        return false;
    };
    let Ok(url) = reqwest::Url::parse_with_params(
        "https://unused.invalid",
        &[
            ("token", token.as_str()),
            ("token_type_hint", "refresh_token"),
        ],
    ) else {
        return false;
    };
    state
        .http
        .post(format!("{}/oauth/revoke", config.api_base))
        .timeout(std::time::Duration::from_secs(REQUEST_SECONDS))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(url.query().unwrap_or_default().to_owned())
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}
