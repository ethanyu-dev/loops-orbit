use super::{CALLBACK, SCOPES, client, configured, crypto};
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
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

// 主会话 Cookie 保持 Strict；仅此短期、回调专用 Cookie 允许跨站顶层返回。
const COOKIE: &str = "orbit_feishu_oauth";
const STATE_SECONDS: i64 = 600;
/// 从指定 Cookie 读取单一凭证，不接受 query 传入会话。
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
/// 用独立随机值绑定发起浏览器；state 泄漏也不能在另一浏览器完成绑定。
pub(super) async fn start(
    State(state): State<AppState>,
    identity: Identity,
    headers: HeaderMap,
) -> ApiResult<Response> {
    identity.require_admin()?;
    configured(&state)?;
    let session = cookie(&headers, "orbit_session").ok_or(ApiError(
        StatusCode::UNAUTHORIZED,
        "authentication_required",
    ))?;
    let nonce = Uuid::new_v4().to_string();
    let browser = Uuid::new_v4().to_string();
    sqlx::query("DELETE FROM communication_oauth_states WHERE expires_at<now() OR session_hash=$1")
        .bind(auth::hash(session))
        .execute(&state.pool)
        .await?;
    sqlx::query("INSERT INTO communication_oauth_states(state_hash,browser_hash,session_hash,expires_at) VALUES($1,$2,$3,now()+make_interval(secs=>$4))").bind(auth::hash(&nonce)).bind(auth::hash(&browser)).bind(auth::hash(session)).bind(STATE_SECONDS as f64).execute(&state.pool).await?;
    let mut url = reqwest::Url::parse("https://accounts.feishu.cn/open-apis/authen/v1/authorize")
        .expect("固定授权地址");
    url.query_pairs_mut()
        .append_pair(
            "client_id",
            &state.config.feishu.as_ref().expect("已验证配置").app_id,
        )
        .append_pair(
            "redirect_uri",
            &format!("{}{CALLBACK}", state.config.api_public_url),
        )
        .append_pair("response_type", "code")
        .append_pair("state", &nonce)
        .append_pair("scope", SCOPES);
    let secure = if state.config.api_public_url.starts_with("https:") {
        "; Secure"
    } else {
        ""
    };
    Ok(([(header::SET_COOKIE,format!("{COOKIE}={browser}; Path={CALLBACK}; HttpOnly; SameSite=Lax; Max-Age={STATE_SECONDS}{secure}"))],Json(json!({"url":url.to_string()}))).into_response())
}
/// 授权拒绝没有 code；state 仍必须校验并一次性消耗。
#[derive(Deserialize)]
pub(super) struct Callback {
    /// 随机防伪参数。
    state: String,
    /// 飞书短期授权码，不持久化或记录日志。
    code: Option<String>,
}
/// 回调验证原始管理员会话仍有效，不降低主会话 Cookie 的站点策略。
pub(super) async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(input): Query<Callback>,
) -> ApiResult<Response> {
    configured(&state)?;
    let browser = cookie(&headers, COOKIE).ok_or(ApiError(
        StatusCode::BAD_REQUEST,
        "communication_oauth_invalid",
    ))?;
    let _guard = state.communications.lock().await;
    let valid: Option<String> = sqlx::query_scalar("DELETE FROM communication_oauth_states o USING sessions s WHERE o.state_hash=$1 AND o.browser_hash=$2 AND o.expires_at>now() AND s.token_hash=o.session_hash AND s.expires_at>now() AND s.grant_id IS NULL AND s.admin_fingerprint=$3 RETURNING o.state_hash")
        .bind(auth::hash(&input.state)).bind(auth::hash(browser)).bind(auth::hash(&state.config.admin_token)).fetch_optional(&state.pool).await?;
    if valid.is_none() {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "communication_oauth_invalid",
        ));
    }
    let result = if let Some(code) = input.code.filter(|s| !s.is_empty() && s.len() < 4096) {
        connect(&state, &code).await
    } else {
        Err(ApiError(
            StatusCode::BAD_REQUEST,
            "communication_oauth_denied",
        ))
    };
    let outcome = if result.is_ok() {
        "connected"
    } else {
        "failed"
    };
    // 返回配置的前端功能页，不把供应商错误、code、state 带回前端或日志。
    Ok((
        [(
            header::SET_COOKIE,
            format!("{COOKIE}=; Path={CALLBACK}; HttpOnly; SameSite=Lax; Max-Age=0"),
        )],
        Redirect::to(&format!(
            "{}/communications?feishu={outcome}",
            state.config.public_url
        )),
    )
        .into_response())
}
/// 身份取自飞书用户信息接口；重新授权不能悄悄把已有资料换成另一账号。
async fn connect(state: &AppState, code: &str) -> ApiResult<()> {
    let (tokens,expires,refresh_expires) = client::exchange(state,json!({"grant_type":"authorization_code","code":code,"redirect_uri":format!("{}{CALLBACK}",state.config.api_public_url)})).await?;
    let data = client::json_response(client::get(
        state,
        "/authen/v1/user_info",
        &tokens.access_token,
    ))
    .await?;
    let open_id = client::string(&data["data"], "open_id")?;
    let name = client::string(&data["data"], "name")?;
    if !state
        .config
        .feishu
        .as_ref()
        .expect("已验证配置")
        .allowed_users
        .contains(&open_id)
    {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "communication_account_not_allowed",
        ));
    }
    let mut tx = state.pool.begin().await?;
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT open_id FROM communication_connections WHERE owner='admin' FOR UPDATE",
    )
    .fetch_optional(&mut *tx)
    .await?;
    if existing.is_some_and(|id| id != open_id) {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "communication_account_changed",
        ));
    }
    sqlx::query("INSERT INTO communication_connections(owner,open_id,name,credentials,expires_at,refresh_expires_at) VALUES('admin',$1,$2,$3,$4,$5) ON CONFLICT(owner) DO UPDATE SET name=excluded.name,credentials=excluded.credentials,expires_at=excluded.expires_at,refresh_expires_at=excluded.refresh_expires_at,status='active',version=communication_connections.version+1")
        .bind(open_id).bind(name).bind(crypto::seal(state,&tokens)?).bind(expires).bind(refresh_expires).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
