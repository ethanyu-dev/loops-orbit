use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::{
    Json,
    extract::{FromRequestParts, State},
    http::{HeaderMap, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

// 管理员会话有效期为七天，访客会话始终受所属链接的到期时间约束。
const SESSION_SECONDS: i64 = 7 * 24 * 3600;
const AUTH_LIMIT_PER_MINUTE: i32 = 30;

// 共享限流的原子更新 SQL，包含过期桶复用规则。
const INCREMENT_RATE_LIMIT_SQL: &str = include_str!("sql/increment_rate_limit.sql");

/// 已认证身份；访客只能访问同一个授权链接创建的会话。
#[derive(Serialize)]
pub struct Identity {
    /// 管理权限由服务端确定，客户端不能指定。
    pub admin: bool,
    /// 数据所有者标识，不等同于浏览器会话 token。
    pub owner: String,
    /// 当前会话最晚的失效时间。
    pub expires_at: DateTime<Utc>,
}
impl Identity {
    /// 管理端点在执行数据库操作前统一检查权限。
    pub fn require_admin(&self) -> ApiResult<()> {
        if self.admin {
            Ok(())
        } else {
            Err(ApiError(StatusCode::FORBIDDEN, "admin_required"))
        }
    }
}
/// 管理员登录或当前已绑定的同一飞书账号才是本人，白名单不构成本人证明。
pub(crate) async fn is_account_owner(state: &AppState, owner: &str) -> ApiResult<bool> {
    Ok(
        crate::todos::identity::principal(state, owner).await? == "admin"
            && crate::followups::policy::owner_allowed(state, owner).await?,
    )
}

/// 联合查询后的认证记录，字段名明确区分会话与授权的有效期。
#[derive(sqlx::FromRow)]
struct SessionRecord {
    /// 访客授权；管理员会话为空。
    grant_id: Option<Uuid>,
    /// 根 token 的摘要用于轮换后使旧管理员会话失效。
    admin_fingerprint: Option<String>,
    /// Cookie 对应的服务端会话到期时间。
    expires_at: DateTime<Utc>,
    /// 关联临时链接的到期时间。
    grant_expires: Option<DateTime<Utc>>,
    /// 非空表示管理员已撤销访问。
    revoked_at: Option<DateTime<Utc>>,
}

impl FromRequestParts<AppState> for Identity {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let token = parts
            .headers
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                v.split(';')
                    .find_map(|p| p.trim().strip_prefix("orbit_session="))
            })
            .ok_or(ApiError(
                StatusCode::UNAUTHORIZED,
                "authentication_required",
            ))?;
        let row: Option<SessionRecord> = sqlx::query_as(
            r#"
                SELECT s.grant_id, s.admin_fingerprint, s.expires_at,
                       g.expires_at AS grant_expires, g.revoked_at
                FROM sessions s
                LEFT JOIN grants g ON g.id = s.grant_id
                WHERE s.token_hash = $1 AND s.expires_at > now()
            "#,
        )
        .bind(hash(token))
        .fetch_optional(&state.pool)
        .await?;
        let SessionRecord {
            grant_id: grant,
            admin_fingerprint: fingerprint,
            expires_at,
            grant_expires,
            revoked_at: revoked,
        } = row.ok_or(ApiError(StatusCode::UNAUTHORIZED, "session_expired"))?;
        if let Some(id) = grant {
            let deadline = grant_expires
                .filter(|d| *d > Utc::now())
                .filter(|_| revoked.is_none())
                .ok_or(ApiError(StatusCode::UNAUTHORIZED, "access_expired"))?;
            Ok(Self {
                admin: false,
                owner: format!("guest:{id}"),
                expires_at: expires_at.min(deadline),
            })
        } else if fingerprint.is_some_and(|v| constant_eq(&v, &hash(&state.config.admin_token))) {
            Ok(Self {
                admin: true,
                owner: "admin".into(),
                expires_at,
            })
        } else {
            Err(ApiError(StatusCode::UNAUTHORIZED, "session_expired"))
        }
    }
}

/// 登录请求不会持久化原始 token。
#[derive(Deserialize)]
pub struct Login {
    /// 管理员根 token 或临时链接 token。
    token: String,
}

/// 管理员 token 兑换随机会话，根 token 不写入 Cookie。
pub async fn login(State(state): State<AppState>, Json(body): Json<Login>) -> ApiResult<Response> {
    auth_limit(&state).await?;
    if !constant_eq(&hash(&body.token), &hash(&state.config.admin_token)) {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "invalid_token"));
    }
    issue_session(
        &state,
        None,
        Utc::now() + chrono::Duration::seconds(SESSION_SECONDS),
    )
    .await
}

/// 临时链接可以在有效期内重复兑换，所有兑换会话关联同一授权。
pub async fn exchange(
    State(state): State<AppState>,
    Json(body): Json<Login>,
) -> ApiResult<Response> {
    auth_limit(&state).await?;
    let grant: Option<(Uuid, DateTime<Utc>)> = sqlx::query_as(
        r#"
        SELECT id, expires_at
        FROM grants
        WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > now()
    "#,
    )
    .bind(hash(&body.token))
    .fetch_optional(&state.pool)
    .await?;
    let (id, deadline) = grant.ok_or(ApiError(
        StatusCode::UNAUTHORIZED,
        "invalid_or_expired_link",
    ))?;
    issue_session(
        &state,
        Some(id),
        deadline.min(Utc::now() + chrono::Duration::seconds(SESSION_SECONDS)),
    )
    .await
}

/// 删除当前会话记录并清除浏览器 Cookie；过期会话同样可以退出。
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    if let Some(token) = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.split(';')
                .find_map(|p| p.trim().strip_prefix("orbit_session="))
        })
    {
        sqlx::query("DELETE FROM sessions WHERE token_hash=$1")
            .bind(hash(token))
            .execute(&state.pool)
            .await?;
    }
    Ok((
        [(header::SET_COOKIE, cookie(&state, "", 0))],
        Json(json!({"ok":true})),
    )
        .into_response())
}

/// 返回浏览器所需的最小身份和模型信息，不包含供应商地址或密钥。
pub async fn me(State(state): State<AppState>, identity: Identity) -> Json<serde_json::Value> {
    Json(json!({
        "identity": identity,
        "model": state.config.model.model,
        "feishu_enabled": state.config.feishu.is_some(),
    }))
}

/// 在数据库中原子计数，避免多实例各自限流被轻易绕过。
async fn auth_limit(state: &AppState) -> ApiResult<()> {
    limit(state, "auth", AUTH_LIMIT_PER_MINUTE).await
}

/// 对固定身份使用每分钟配额；过期桶原地复用，避免每次请求创建新行。
pub async fn limit(state: &AppState, bucket: &str, maximum: i32) -> ApiResult<()> {
    let count: i32 = sqlx::query_scalar(INCREMENT_RATE_LIMIT_SQL)
        .bind(bucket)
        .fetch_one(&state.pool)
        .await?;
    if count > maximum {
        Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "rate_limited"))
    } else {
        Ok(())
    }
}

/// 使用两个 UUID v4 生成含 244 位随机熵的凭证，数据库中仅保存 SHA-256 摘要。
async fn issue_session(
    state: &AppState,
    grant: Option<Uuid>,
    expires_at: DateTime<Utc>,
) -> ApiResult<Response> {
    let token = new_token();
    sqlx::query(
        r#"
        INSERT INTO sessions (token_hash, grant_id, admin_fingerprint, expires_at)
        VALUES ($1, $2, $3, $4)
    "#,
    )
    .bind(hash(&token))
    .bind(grant)
    .bind(if grant.is_none() {
        Some(hash(&state.config.admin_token))
    } else {
        None
    })
    .bind(expires_at)
    .execute(&state.pool)
    .await?;
    Ok((
        [(
            header::SET_COOKIE,
            cookie(
                state,
                &token,
                (expires_at - Utc::now()).num_seconds().max(0),
            ),
        )],
        Json(json!({"ok":true})),
    )
        .into_response())
}
/// Cookie 仅属于 API 主机；前后端需同站点，跨源请求显式携带凭据。
fn cookie(state: &AppState, token: &str, age: i64) -> String {
    format!(
        "orbit_session={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={age}{}",
        if state.config.api_public_url.starts_with("https:") {
            "; Secure"
        } else {
            ""
        }
    )
}
/// 两个独立 UUID 提供足够的随机熵，不包含可推断的用户信息。
pub fn new_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
/// token 摘要用于查询，日志和响应均不输出原始 token。
pub fn hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
/// 比较固定长度摘要，避免逐字符提前返回。
pub fn constant_eq(a: &str, b: &str) -> bool {
    bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}
