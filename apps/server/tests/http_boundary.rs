use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use orbit_server::{AppState, config::Config, router};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

// 仅用于检查 HTTP 边界；连接池不连接数据库，任何依赖数据库的端点另由集成测试覆盖。
const FRONTEND: &str = "https://orbit.ethankit.com";

/// 构造生产路由但不启动 worker，不依赖网络、真实凭据或生产数据。
fn app() -> Router {
    let config = Config {
        linear: None,
        database_url: "postgres://unused:unused@localhost/unused".into(),
        admin_token: "http-boundary-test-token-at-least-32-bytes".into(),
        public_url: FRONTEND.into(),
        api_public_url: "https://api-orbit.ethankit.com".into(),
        port: 0,
        workers: 1,
        model: agent_runtime::ModelConfig {
            base_url: "http://localhost:1".into(),
            model: "unused".into(),
            api_key: "unused".into(),
            stream_enabled: false,
            tools_enabled: false,
        },
        feishu: None,
        memory: None,
        communications: None,
        followup_timezone: "Asia/Shanghai".into(),
    };
    let pool = PgPoolOptions::new()
        .connect_lazy(&config.database_url)
        .unwrap();
    router(AppState::new(config, pool).unwrap())
}

// 验证凭据请求预检仅放行固定前端；不模拟浏览器的 SameSite 或 DNS/TLS 行为。
#[tokio::test]
async fn credential_preflight_is_limited_to_frontend() {
    for origin in [
        FRONTEND,
        "https://evil.example",
        "https://orbit.ethankit.com.evil.example",
        "null",
    ] {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/api/auth/login")
                    .header("origin", origin)
                    .header("access-control-request-method", "POST")
                    .header("access-control-request-headers", "content-type")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success());
        if origin == FRONTEND {
            assert_eq!(response.headers()["access-control-allow-origin"], FRONTEND);
            assert_eq!(
                response.headers()["access-control-allow-credentials"],
                "true"
            );
            assert!(
                response.headers()["access-control-allow-methods"]
                    .to_str()
                    .unwrap()
                    .contains("POST")
            );
            assert_eq!(
                response.headers()["access-control-allow-headers"],
                "content-type"
            );
        } else {
            assert!(
                response
                    .headers()
                    .get("access-control-allow-origin")
                    .is_none()
            );
        }
    }
}

// 验证 API 错误也有 CORS 头，前端能识别未登录；不验证已登录会话查询。
#[tokio::test]
async fn unauthenticated_errors_are_readable_by_frontend() {
    let response = app()
        .oneshot(
            Request::builder()
                .uri("/api/me")
                .header("origin", FRONTEND)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()["access-control-allow-origin"], FRONTEND);
    assert_eq!(response.headers()["cache-control"], "no-store");
}

// 验证跨域启用后仍拒绝无来源或恶意来源写入；不覆盖飞书签名校验。
#[tokio::test]
async fn writes_still_require_exact_origin() {
    for origin in [None, Some("https://evil.example")] {
        let mut request = Request::builder().method("POST").uri("/api/auth/logout");
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        let response = app()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"],
            "invalid_origin"
        );
    }
}

// 验证后端不再提供 SPA 回退，未知 API 与页面均返回 JSON 404；不检查 Nginx。
#[tokio::test]
async fn api_service_never_serves_frontend_pages() {
    for path in [
        "/",
        "/chat",
        "/memory",
        "/api/unknown",
        "/assets/missing.js",
    ] {
        let response = app()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()["content-type"], "application/json");
    }
}
