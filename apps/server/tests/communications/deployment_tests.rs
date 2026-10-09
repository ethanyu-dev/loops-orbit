use super::*;

// 验证真实路由生成 API 回调、前端返回地址和跨源 Cookie；飞书交换使用本地夹具，不覆盖 DNS/TLS 或租户授权。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn oauth_uses_api_origin_and_returns_to_frontend() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    let (status, oauth, value) = h
        .request(
            "POST",
            "/api/communications/oauth/start",
            Some(&cookie),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let url = reqwest::Url::parse(value["url"].as_str().unwrap()).unwrap();
    let params: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(
        params["redirect_uri"],
        "http://localhost:8080/api/communications/oauth/callback"
    );
    // 授权 URL 申请发送权限，实际是否获得权限仍以 token 响应为准。
    let scopes: Vec<_> = params["scope"].split_whitespace().collect();
    assert!(scopes.contains(&"im:message"));
    assert!(scopes.contains(&"im:message.send_as_user"));
    let response = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/communications/oauth/callback?state={}&code=fixture",
                    params["state"]
                ))
                .header("cookie", oauth.unwrap())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        format!("{ORIGIN}/communications?feishu=connected")
    );
    server.abort();
    h.close().await;
}
