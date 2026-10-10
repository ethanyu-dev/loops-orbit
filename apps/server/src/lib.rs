pub mod api;
pub mod auth;
pub mod communications;
pub mod config;
pub mod context;
pub mod error;
pub mod feishu;
pub mod followups;
pub mod knowledge;
pub mod linear;
pub mod memory;
pub mod rag;
pub mod todos;
pub mod tools;
pub mod worker;

use crate::{
    config::Config,
    error::{ApiError, ApiResult},
};
use axum::{
    Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use sqlx::PgPool;
use std::{sync::Arc, time::Duration};
use tower_http::cors::CorsLayer;

// JSON 与飞书加密载荷共同受此限制；真实消息文本另有字符数上限。
const MAX_BODY_BYTES: usize = 256 * 1024;

// 分段保持安全策略可审阅，concat! 不改变最终响应头中的空格和指令。
const CONTENT_SECURITY_POLICY: &str = concat!(
    "default-src 'self'; ",
    "script-src 'self'; ",
    "style-src 'self' 'unsafe-inline'; ",
    "img-src 'self' data:; ",
    "connect-src 'self'; ",
    "frame-ancestors 'none'; ",
    "base-uri 'none'; ",
    "form-action 'self'",
);

/// 可克隆的应用状态，原文记忆位于本地持久卷，其余状态位于 PostgreSQL。
#[derive(Clone)]
pub struct AppState {
    /// 每实例有限容量的连接池。
    pub pool: PgPool,
    /// 不在日志中格式化输出的启动配置。
    pub config: Arc<Config>,
    /// 不依赖特定框架的自研模型执行器。
    pub runtime: agent_runtime::Runtime,
    /// 飞书请求共享客户端。
    pub http: reqwest::Client,
    /// 单实例记忆写入锁与身份隔离的检索缓存。
    pub memory: memory::SharedMemory,
    /// 连接、文件提交与遗忘的单实例写入锁；网络采集在锁外进行。
    pub communications: Arc<tokio::sync::Mutex<()>>,
}
impl AppState {
    /// 测试与生产复用完全一致的应用组装方式。
    pub fn new(config: Config, pool: PgPool) -> anyhow::Result<Self> {
        Ok(Self {
            communications: Default::default(),
            memory: Default::default(),
            runtime: agent_runtime::Runtime::new(config.model.clone())?,
            pool,
            config: Arc::new(config),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .connect_timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }
}

/// 只提供 API；跨域凭据仅允许配置的前端源，写操作继续单独验证 Origin。
pub fn router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin([HeaderValue::from_str(&state.config.public_url).expect("已验证前端源")])
        .allow_credentials(true)
        .allow_methods([
            Method::GET,
            Method::HEAD,
            Method::POST,
            Method::PUT,
            Method::DELETE,
        ])
        .allow_headers([header::CONTENT_TYPE])
        .expose_headers([header::HeaderName::from_static("x-request-id")])
        .max_age(Duration::from_secs(600));
    Router::new()
        .route("/health/live", get(|| async { "ok" }))
        .route("/health/ready", get(ready))
        .route("/metrics", get(metrics))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/exchange", post(auth::exchange))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/me", get(auth::me))
        .nest("/api/communications", communications::routes::router())
        .nest("/api/linear", linear::routes::router())
        .nest("/api/todos", todos::routes::router())
        .nest("/api/knowledge", knowledge::routes::router())
        .route(
            "/api/followups",
            get(followups::routes::index).post(followups::routes::create),
        )
        .route(
            "/api/followups/preferences",
            axum::routing::put(followups::routes::prefs),
        )
        .route(
            "/api/followups/{id}",
            axum::routing::put(followups::routes::update),
        )
        .route("/api/notifications", get(followups::routes::notifications))
        .route(
            "/api/notifications/{id}/read",
            post(followups::routes::read),
        )
        .route(
            "/api/memories",
            get(memory::routes::index).post(memory::routes::create),
        )
        .route(
            "/api/memories/{id}",
            axum::routing::put(memory::routes::update).delete(memory::routes::remove),
        )
        .route(
            "/api/conversations",
            get(api::conversations).post(api::create_conversation),
        )
        .route("/api/conversations/{id}", get(api::detail))
        .route("/api/conversations/{id}/messages", post(api::send))
        .route(
            "/api/conversations/{id}/runs/{run_id}/cancel",
            post(api::cancel),
        )
        .route("/api/admin/links", get(api::links).post(api::create_link))
        .route("/api/admin/links/{id}", delete(api::revoke_link))
        .route("/api/admin/status", get(api::status))
        .route("/api/channels/feishu/events", post(feishu::webhook))
        .fallback(|| async { ApiError(StatusCode::NOT_FOUND, "not_found") })
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), boundary))
        // 位于来源校验之外，让 401/403 等错误也能被可信前端读取。
        .layer(cors)
        .with_state(state)
}

/// 活性不访问数据库，就绪检查验证数据库连接，供应商短暂故障不触发全实例重启。
async fn ready(State(state): State<AppState>) -> ApiResult<&'static str> {
    sqlx::query("SELECT 1").execute(&state.pool).await?;
    Ok("ok")
}

/// 指标端点使用根 token 的 Bearer 认证，不公开运行统计。
async fn metrics(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Response> {
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if !auth::constant_eq(
        &auth::hash(supplied),
        &auth::hash(&state.config.admin_token),
    ) {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "authentication_required",
        ));
    }
    let data = api::snapshot(&state).await?;
    let mut output = String::from(
        "# HELP orbit_runs Number of persisted agent runs by status.\n# TYPE orbit_runs gauge\n",
    );
    for status in [
        "queued",
        "running",
        "completed",
        "failed",
        "cancelled",
        "superseded",
    ] {
        output.push_str(&format!(
            "orbit_runs{{status=\"{status}\"}} {}\n",
            data["runs"][status]
        ));
    }
    output.push_str(concat!(
        "# HELP orbit_deliveries Number of Feishu deliveries by status.\n",
        "# TYPE orbit_deliveries gauge\n",
    ));
    for status in ["queued", "running", "completed", "failed"] {
        output.push_str(&format!(
            "orbit_deliveries{{status=\"{status}\"}} {}\n",
            data["delivery"][status]
        ));
    }
    Ok((
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        output,
    )
        .into_response())
}

/// 检查浏览器来源、限制请求耗时，并记录无凭证和正文的结构化访问日志。
async fn boundary(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let request_id = uuid::Uuid::new_v4().to_string();
    let started = std::time::Instant::now();
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    let mut response = if path.starts_with("/api/")
        && !matches!(method.as_str(), "GET" | "HEAD" | "OPTIONS")
        && path != "/api/channels/feishu/events"
        && request
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            != Some(state.config.public_url.as_str())
    {
        ApiError(StatusCode::FORBIDDEN, "invalid_origin").into_response()
    } else {
        tokio::time::timeout(Duration::from_secs(15), next.run(request))
            .await
            .unwrap_or_else(|_| {
                ApiError(StatusCode::GATEWAY_TIMEOUT, "request_timeout").into_response()
            })
    };
    let headers = response.headers_mut();
    headers.insert("x-request-id", HeaderValue::from_str(&request_id).unwrap());
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
    if path.starts_with("/api/") || path == "/metrics" {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    if state.config.api_public_url.starts_with("https:") {
        headers.insert(
            "strict-transport-security",
            HeaderValue::from_static("max-age=31536000"),
        );
    }
    // 不记录 query、Cookie、Authorization、正文和模型上游响应。
    tracing::info!(
        %request_id,
        %method,
        %path,
        status = response.status().as_u16(),
        duration_ms = started.elapsed().as_millis() as u64,
        "HTTP 请求完成",
    );
    response
}
