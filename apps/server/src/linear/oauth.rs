use super::{CALLBACK, Connection, client, configured, credentials, invalid, unavailable};
use crate::{
    AppState,
    auth::{self, Identity},
    error::{ApiError, ApiResult},
};
use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

// 独立 Lax Cookie 仅用于 OAuth 回调，主登录 Cookie 继续保持 Strict。
const COOKIE: &str = "orbit_linear_oauth";
const STATE_SECONDS: i64 = 600;
/// 精确匹配 Cookie 名称，不接受查询字符串传入登录凭证。
fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| {
            part.trim()
                .split_once('=')
                .filter(|(key, _)| *key == name)
                .map(|(_, value)| value)
        })
}
/// 写权限必须在发起授权时显式选择；不要求管理整个 Linear 工作空间。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Start {
    /// false 只申请 read，true 同时申请 write。
    #[serde(default)]
    write: bool,
}
/// 授权 state 同时绑定浏览器、有效管理员会话及 PKCE verifier。
pub(super) async fn start(
    State(state): State<AppState>,
    identity: Identity,
    headers: HeaderMap,
    Json(input): Json<Start>,
) -> ApiResult<Response> {
    identity.require_admin()?;
    let config = configured(&state)?;
    let session = cookie(&headers, "orbit_session").ok_or_else(invalid)?;
    let nonce = Uuid::new_v4().to_string();
    let browser = Uuid::new_v4().to_string();
    let verifier = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut tx = state.pool.begin().await?;
    sqlx::query("DELETE FROM linear_oauth_states WHERE expires_at<now() OR session_hash=$1")
        .bind(auth::hash(session))
        .execute(&mut *tx)
        .await?;
    sqlx::query(include_str!("../sql/linear_oauth_start.sql"))
        .bind(auth::hash(&nonce))
        .bind(auth::hash(&browser))
        .bind(auth::hash(session))
        .bind(verifier)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let mut url = reqwest::Url::parse("https://linear.app/oauth/authorize").expect("固定授权地址");
    url.query_pairs_mut()
        .append_pair("client_id", &config.client_id)
        .append_pair(
            "redirect_uri",
            &format!("{}{CALLBACK}", state.config.api_public_url),
        )
        .append_pair("response_type", "code")
        .append_pair("scope", if input.write { "read,write" } else { "read" })
        .append_pair("actor", "user")
        .append_pair("state", &nonce)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("prompt", "consent");
    let secure = if state.config.api_public_url.starts_with("https:") {
        "; Secure"
    } else {
        ""
    };
    Ok(([(header::SET_COOKIE,format!("{COOKIE}={browser}; Path={CALLBACK}; HttpOnly; SameSite=Lax; Max-Age={STATE_SECONDS}{secure}"))],Json(json!({"url":url.to_string()}))).into_response())
}
/// 供应商拒绝授权也需要消耗有效 state，不能重复利用旧回调。
#[derive(Deserialize)]
pub(super) struct Callback {
    /// 发起时的随机值。
    state: String,
    /// 短期授权码，不记录或返回给前端。
    code: Option<String>,
}
/// 回调不会使用跨站主 Cookie，改为校验 state 绑定的原始管理员会话。
pub(super) async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(input): Query<Callback>,
) -> ApiResult<Response> {
    configured(&state)?;
    let browser = cookie(&headers, COOKIE)
        .ok_or(ApiError(StatusCode::BAD_REQUEST, "linear_oauth_invalid"))?;
    let verifier: Option<(String, i64, String)> =
        sqlx::query_as(include_str!("../sql/linear_oauth_consume.sql"))
            .bind(auth::hash(&input.state))
            .bind(auth::hash(browser))
            .bind(auth::hash(&state.config.admin_token))
            .fetch_optional(&state.pool)
            .await?;
    let (verifier, version, session_hash) =
        verifier.ok_or(ApiError(StatusCode::BAD_REQUEST, "linear_oauth_invalid"))?;
    let result = match input.code.filter(|s| !s.is_empty() && s.len() < 4096) {
        Some(code) => connect(&state, code, verifier, version, session_hash).await,
        None => Err(ApiError(StatusCode::BAD_REQUEST, "linear_oauth_denied")),
    };
    let outcome = match result {
        Ok(()) => "connected",
        Err(error) if error.1 == "linear_account_changed" => "account_changed",
        Err(_) => "failed",
    };
    Ok((
        [(
            header::SET_COOKIE,
            format!("{COOKIE}=; Path={CALLBACK}; HttpOnly; SameSite=Lax; Max-Age=0"),
        )],
        Redirect::to(&format!(
            "{}/integrations?linear={outcome}",
            state.config.public_url
        )),
    )
        .into_response())
}
/// 账号与工作空间由服务端 API 确认，重连不能静默切换身份。
async fn connect(
    state: &AppState,
    code: String,
    verifier: String,
    version: i64,
    session_hash: String,
) -> ApiResult<()> {
    let config = configured(state)?;
    let (tokens, expires, scopes) = client::exchange(
        state,
        &[
            ("grant_type", "authorization_code".into()),
            ("code", code),
            ("code_verifier", verifier),
            (
                "redirect_uri",
                format!("{}{CALLBACK}", state.config.api_public_url),
            ),
        ],
    )
    .await?;
    let scopes = scopes.ok_or_else(|| unavailable("授权范围缺失"))?;
    if !scopes.iter().any(|s| s == "read") {
        return Err(unavailable("缺少读取权限"));
    }
    let data = client::graphql(
        state,
        &tokens.access_token,
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
    let user = field("viewer", "id")?;
    let workspace = field("organization", "id")?;
    let slug = field("organization", "urlKey")?;
    if config
        .workspace_slug
        .as_ref()
        .is_some_and(|expected| expected != &slug)
    {
        return Err(ApiError(StatusCode::CONFLICT, "linear_account_changed"));
    }
    let mut tx = state.pool.begin().await?;
    // 没有连接行时也串行化重连，避免两个授权回调互相覆盖账号。
    sqlx::query("SELECT pg_advisory_xact_lock(71392311)")
        .execute(&mut *tx)
        .await?;
    let current_version: i64 =
        sqlx::query_scalar("SELECT version FROM linear_guard WHERE id=true FOR UPDATE")
            .fetch_one(&mut *tx)
            .await?;
    let session_valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sessions WHERE token_hash=$1 AND expires_at>now() AND grant_id IS NULL AND admin_fingerprint=$2)").bind(session_hash).bind(auth::hash(&state.config.admin_token)).fetch_one(&mut *tx).await?;
    if current_version != version || !session_valid {
        return Err(ApiError(StatusCode::CONFLICT, "linear_oauth_invalid"));
    }
    let current: Option<Connection> =
        sqlx::query_as("SELECT * FROM linear_connections WHERE owner='admin' FOR UPDATE")
            .fetch_optional(&mut *tx)
            .await?;
    if current.is_some_and(|current| current.user_id != user || current.workspace_id != workspace) {
        return Err(ApiError(StatusCode::CONFLICT, "linear_account_changed"));
    }
    sqlx::query(include_str!("../sql/linear_connection_save.sql"))
        .bind(Uuid::new_v4())
        .bind(user)
        .bind(field("viewer", "name")?)
        .bind(workspace)
        .bind(field("organization", "name")?)
        .bind(slug)
        .bind(credentials::seal(state, &tokens)?)
        .bind(expires)
        .bind(scopes)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE linear_guard SET version=version+1 WHERE id=true")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
