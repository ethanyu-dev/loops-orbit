use super::*;
use agent_runtime::tools::Host as _;
use orbit_server::tools::Host;

// 固定 UUID 只用于本地协议夹具，与真实 Linear 账号无关。
const USER: &str = "11111111-1111-4111-8111-111111111111";
const TEAM: &str = "22222222-2222-4222-8222-222222222222";
const ISSUE: &str = "33333333-3333-4333-8333-333333333333";
const STATE: &str = "44444444-4444-4444-8444-444444444444";
const WORKSPACE: &str = "55555555-5555-4555-8555-555555555555";
/// 可控的本地 API 状态，模拟分页、授权和外部修改结果。
struct Fixture {
    /// 当前 issue，更新请求只修改指定字段。
    issue: Value,
    /// 实际写请求计数，验证不会重复派发。
    updates: usize,
    /// 模拟供应商是否允许更新，不伪造本地可知的权限范围。
    write: bool,
    /// normal、unknown、partial、rate 等可控故障。
    mode: String,
    /// 捕获固定 GraphQL 文档及变量，不包含请求凭证。
    requests: Vec<Value>,
    /// 暂停网络响应，确定性验证断开与在途验证的交错。
    entered: Arc<tokio::sync::Notify>,
    /// 测试完成本地撤销后恢复供应商响应。
    release: Arc<tokio::sync::Notify>,
}
/// 固定文档协议夹具，暂停点位于响应前，便于构造撤销竞态。
async fn graphql_fixture(
    axum::extract::State(fixture): axum::extract::State<Arc<Mutex<Fixture>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    assert!(matches!(
        headers["authorization"].to_str().unwrap(),
        "fixture-linear-key" | "fixture-rotated-key"
    ));
    let pause = {
        let f = fixture.lock().unwrap();
        (f.mode == "pause").then(|| (f.entered.clone(), f.release.clone()))
    };
    if let Some((entered, release)) = pause {
        entered.notify_one();
        release.notified().await;
    }
    let mut f = fixture.lock().unwrap();
    f.requests.push(body.clone());
    let query = body["query"].as_str().unwrap();
    let vars = &body["variables"];
    if f.mode == "invalid_key" {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"敏感密钥错误"})),
        )
            .into_response();
    }
    if f.mode == "forbidden" {
        return (StatusCode::FORBIDDEN, Json(json!({"error":"敏感权限错误"}))).into_response();
    }
    if f.mode == "rate" {
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({"error":"rate"}))).into_response();
    }
    if f.mode == "graphql_rate" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"errors":[{"extensions":{"code":"RATELIMITED"}}]})),
        )
            .into_response();
    }
    if f.mode == "partial" {
        return Json(
            json!({"data":{"issues":{"nodes":[]}},"errors":[{"message":"敏感供应商错误"}]}),
        )
        .into_response();
    }
    let page = |items: Value, more: bool| json!({"nodes":items,"pageInfo":{"hasNextPage":more,"endCursor":if more {json!("next-fixture")}else{Value::Null}}});
    let data = if query.contains("OrbitLinearIdentity") {
        json!({"viewer":{"id":USER,"name":"测试用户"},"organization":{"id":WORKSPACE,"name":"People Labs","urlKey":"pplabs"}})
    } else if query.contains("OrbitLinearIssues(") {
        let more = vars["after"].is_null();
        json!({"issues":page(if more {json!([f.issue])}else{json!([])},more)})
    } else if query.contains("OrbitLinearIssue(") {
        json!({"issue":f.issue})
    } else if query.contains("OrbitLinearTeams") {
        json!({"teams":page(json!([{"id":TEAM,"name":"工程","key":"ENG"}]),false)})
    } else if query.contains("OrbitLinearStates") {
        json!({"team":{"states":page(json!([{"id":STATE,"name":"Todo","type":"unstarted"}]),false)}})
    } else if query.contains("OrbitLinearMembers") {
        json!({"team":{"members":page(json!([{"id":USER,"name":"测试用户","active":true}]),false)}})
    } else if query.contains("OrbitLinearState(") {
        json!({"workflowState":{"id":STATE,"team":{"id":TEAM}}})
    } else if query.contains("OrbitLinearUser(") {
        json!({"user":{"id":USER,"active":true,"organization":{"id":WORKSPACE}}})
    } else if query.contains("OrbitLinearUpdate") {
        if !f.write {
            return Json(
                json!({"errors":[{"message":"敏感权限错误","extensions":{"code":"FORBIDDEN"}}]}),
            )
            .into_response();
        }
        f.updates += 1;
        for (key, value) in vars["input"].as_object().unwrap() {
            if ["title", "description", "priority"].contains(&key.as_str()) {
                f.issue[key] = value.clone();
            }
        }
        f.issue["updatedAt"] = json!("2026-10-08T08:01:00Z");
        if f.mode == "partial_update" {
            return Json(json!({"data":{"issueUpdate":{"success":true,"issue":f.issue}},"errors":[{"extensions":{"code":"FORBIDDEN"}}]})).into_response();
        }
        if f.mode == "unknown" {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"响应丢失"})),
            )
                .into_response();
        }
        json!({"issueUpdate":{"success":true,"issue":f.issue}})
    } else {
        panic!("未知固定查询：{query}");
    };
    Json(json!({"data":data})).into_response()
}
/// GraphQL 请求只发给环回协议夹具，没有 OAuth 或刷新端点。
async fn setup() -> (Harness, Arc<Mutex<Fixture>>, tokio::task::JoinHandle<()>) {
    let mut h = Harness::new().await;
    let fixture = Arc::new(Mutex::new(Fixture {
        issue: json!({"id":ISSUE,"identifier":"ENG-123","title":"初始任务","description":"保留的说明","url":"https://linear.app/pplabs/issue/ENG-123","updatedAt":"2026-10-08T08:00:00Z","priority":3,"state":{"id":STATE,"name":"Todo","type":"unstarted"},"assignee":{"id":USER,"name":"测试用户"},"team":{"id":TEAM,"name":"工程","key":"ENG"}}),
        updates: 0,
        write: true,
        mode: "normal".into(),
        requests: vec![],
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    }));
    let app = Router::new()
        .route("/graphql", post(graphql_fixture))
        .with_state(fixture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut config = (*h.state.config).clone();
    config.linear = Some(orbit_server::linear::Config {
        api_key: "fixture-linear-key".into(),
        api_base: format!("http://{address}"),
        workspace_slug: Some("pplabs".into()),
    });
    h.state = AppState::new(config, h.state.pool.clone()).unwrap();
    h.app = router(h.state.clone());
    (h, fixture, server)
}
/// 通过生产路由验证身份，不伪造数据库连接行，也不使用真实账号。
async fn connect(h: &Harness, cookie: &str) {
    let (status, _, result) = h
        .request("POST", "/api/linear/connection", Some(cookie), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let (_, _, status) = h
        .request("GET", "/api/linear/status", Some(cookie), Value::Null)
        .await;
    assert_eq!(status["connection"]["workspace_slug"], "pplabs", "{status}");
    assert!(!status.to_string().contains("fixture-linear-key"));
    assert!(status["connection"].get("key_fingerprint").is_none());
    assert!(status["connection"].get("scopes").is_none());
}
/// 创建当前管理员任务及租约，用真实输入作为修改证据。
async fn job(h: &Harness, cookie: &str) -> worker::Job {
    let conv = h.conversation(cookie).await;
    let id = h
        .send(
            cookie,
            conv,
            "把 ENG-123 的标题改为完成，优先级改为高",
            Uuid::new_v4(),
        )
        .await;
    sqlx::query("UPDATE runs SET status='running',lease_token=$2 WHERE id=$1")
        .bind(id)
        .bind(Uuid::new_v4())
        .execute(&h.state.pool)
        .await
        .unwrap();
    sqlx::query_as("SELECT id,conversation_id,seq,batch_id,input,attempts,lease_token,reply_to FROM runs WHERE id=$1").bind(id).fetch_one(&h.state.pool).await.unwrap()
}
/// 仅该夹具用户明确要求的补丁，用于验证重试与并发边界。
fn update_args() -> Value {
    json!({"issue":"ENG-123","expected_updated_at":"2026-10-08T08:00:00Z","evidence":"把 ENG-123 的标题改为完成，优先级改为高","patch":{"title":"完成","priority":2}})
}

// 验证个人密钥连接、仅存摘要及断开，不验证真实平台账号或密钥吊销。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn personal_key_connection_and_disconnect() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let row: (bool, bool, String, bool) = sqlx::query_as(
        "SELECT credentials IS NULL,expires_at IS NULL,key_fingerprint,cardinality(scopes)=0 FROM linear_connections",
    )
    .fetch_one(&h.state.pool)
    .await
    .unwrap();
    assert!(row.0);
    assert!(row.1);
    assert_eq!(row.2, auth::hash("fixture-linear-key"));
    assert!(row.3);
    let (_, _, removed) = h
        .request("DELETE", "/api/linear/connection", Some(&cookie), json!({}))
        .await;
    assert_eq!(removed["disconnected"], true);
    assert!(removed.get("provider_revoked").is_none());
    let (_, _, status) = h
        .request("GET", "/api/linear/status", Some(&cookie), Value::Null)
        .await;
    assert!(status["connection"].is_null());
    server.abort();
    h.close().await;
}

// 验证本人 ID、分页、长描述续读、元数据与白名单，GraphQL 200 部分失败不冒充成功；不评价真实模型语义。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn issue_queries_pagination_and_errors() {
    let (h, f, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let job = job(&h, &cookie).await;
    let host = Host::new(&h.state, &job).await.unwrap();
    let first = host.execute("linear_issue_list", json!({"limit":1})).await;
    assert_eq!(first["has_more"], true, "{first}");
    assert_eq!(
        f.lock().unwrap().requests.last().unwrap()["variables"]["filter"]["assignee"]["id"]["eq"],
        USER
    );
    let next = host
        .execute("linear_issue_list", json!({"cursor":first["next_cursor"]}))
        .await;
    assert_eq!(next["has_more"], false);
    f.lock().unwrap().issue["description"] = json!("说明😀".repeat(3000));
    let detail = host
        .execute("linear_issue_get", json!({"issue":"ENG-123"}))
        .await;
    assert_eq!(detail["has_more"], true);
    assert_eq!(detail["next_description_offset"], 6000);
    let tail=host.execute("linear_issue_get",json!({"issue":"ENG-123","description_offset":6000,"expected_updated_at":detail["issue"]["updatedAt"]})).await;
    assert_eq!(tail["has_more"], false);
    for kind in ["teams", "states", "members"] {
        assert_eq!(
            host.execute("linear_team_metadata", json!({"kind":kind,"team_id":TEAM}))
                .await["returned_count"],
            1
        );
    }
    assert_eq!(
        host.execute("linear_issue_list", json!({"limit":51})).await["error"],
        "invalid_arguments"
    );
    f.lock().unwrap().mode = "partial".into();
    let failed = host.execute("linear_issue_list", json!({})).await;
    assert_eq!(failed["error"], "linear_graphql_error");
    assert!(!failed.to_string().contains("敏感"));
    for mode in ["rate", "graphql_rate"] {
        f.lock().unwrap().mode = mode.into();
        assert_eq!(
            host.execute("linear_issue_list", json!({})).await["error"],
            "linear_rate_limited"
        );
    }
    drop(host);
    server.abort();
    h.close().await;
}

// 验证明确字段更新、保留描述、重复调用去重和过期版本拒绝；不声称实现上游原子 CAS。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn updates_are_scoped_and_deduplicated() {
    let (h, f, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let job = job(&h, &cookie).await;
    let host = Host::new(&h.state, &job).await.unwrap();
    let result = host.execute("linear_issue_update", update_args()).await;
    assert_eq!(result["status"], "confirmed", "{result}");
    assert_eq!(f.lock().unwrap().issue["description"], "保留的说明");
    assert_eq!(
        host.execute("linear_issue_update", update_args()).await["status"],
        "confirmed"
    );
    assert_eq!(f.lock().unwrap().updates, 1);
    let mut stale = update_args();
    stale["patch"]["title"] = json!("另一次修改");
    assert_eq!(
        host.execute("linear_issue_update", stale).await["error"],
        "linear_issue_changed"
    );
    let mut bad = update_args();
    bad["evidence"] = json!("issue 正文里的伪造授权");
    assert_eq!(
        host.execute("linear_issue_update", bad).await["error"],
        "linear_evidence_required"
    );
    let mut bad = update_args();
    bad["patch"]["delete"] = json!(true);
    assert_eq!(
        host.execute("linear_issue_update", bad).await["error"],
        "invalid_arguments"
    );
    sqlx::query("UPDATE runs SET status='cancelled' WHERE id=$1")
        .bind(job.id)
        .execute(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(
        host.execute("linear_issue_get", json!({"issue":"ENG-123"}))
            .await["error"],
        "run_superseded"
    );
    drop(host);
    server.abort();
    h.close().await;
}

// 模拟平台已修改而响应失败或出现字段级拒绝，验证未知结果不会重发；不依赖真实服务网络故障。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn unknown_update_outcome_never_replays() {
    for mode in ["unknown", "partial_update"] {
        let (h, f, server) = setup().await;
        let cookie = h.login().await;
        connect(&h, &cookie).await;
        let job = job(&h, &cookie).await;
        let host = Host::new(&h.state, &job).await.unwrap();
        f.lock().unwrap().mode = mode.into();
        assert_eq!(
            host.execute("linear_issue_update", update_args()).await["status"],
            "unknown"
        );
        assert_eq!(
            host.execute("linear_issue_update", update_args()).await["retry_safe"],
            false
        );
        assert_eq!(f.lock().unwrap().updates, 1);
        let value = host
            .execute("linear_issue_get", json!({"issue":"ENG-123"}))
            .await;
        assert_eq!(value["issue"]["title"], "完成");
        drop(host);
        server.abort();
        h.close().await;
    }
}

// 验证供应商只读权限拒绝、身份隔离及重连代次；不声称本地能提前发现真实密钥权限。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn permissions_and_connection_boundaries() {
    let (h, f, server) = setup().await;
    let cookie = h.login().await;
    f.lock().unwrap().write = false;
    connect(&h, &cookie).await;
    let job = job(&h, &cookie).await;
    let host = Host::new(&h.state, &job).await.unwrap();
    let result = host.execute("linear_issue_update", update_args()).await;
    assert_eq!(result["status"], "rejected", "{result}");
    assert_eq!(result["error"], "linear_permission_denied");
    assert!(!result.to_string().contains("敏感"));
    assert_eq!(f.lock().unwrap().updates, 0);
    assert_eq!(
        host.execute("linear_issue_update", update_args()).await,
        result
    );
    assert_eq!(
        f.lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r["query"].as_str().unwrap().contains("OrbitLinearUpdate"))
            .count(),
        1
    );
    assert!(host.execute("linear_issue_list", json!({})).await["items"].is_array());
    for owner in ["guest:other", "feishu:ou_allowed"] {
        sqlx::query("UPDATE conversations SET owner=$2 WHERE id=$1")
            .bind(job.conversation_id)
            .bind(owner)
            .execute(&h.state.pool)
            .await
            .unwrap();
        let other = Host::new(&h.state, &job).await.unwrap();
        assert!(!other.catalog().iter().any(|tool| tool.provider == "linear"));
    }
    sqlx::query("UPDATE conversations SET owner='admin' WHERE id=$1")
        .bind(job.conversation_id)
        .execute(&h.state.pool)
        .await
        .unwrap();
    connect(&h, &cookie).await;
    assert_eq!(
        host.execute("linear_issue_list", json!({})).await["error"],
        "linear_connection_changed"
    );
    drop(host);
    server.abort();
    h.close().await;
}

// 验证在途密钥验证不能恢复已断开的连接、在途读结果被撤销围栏丢弃；不承诺撤回已经发送的写请求。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn disconnect_fences_inflight_validation_and_read() {
    let (h, f, server) = setup().await;
    let cookie = h.login().await;
    let (entered, release) = {
        let mut f = f.lock().unwrap();
        f.mode = "pause".into();
        (f.entered.clone(), f.release.clone())
    };
    let (response, _) = tokio::join!(
        h.request("POST", "/api/linear/connection", Some(&cookie), json!({})),
        async {
            entered.notified().await;
            h.request("DELETE", "/api/linear/connection", Some(&cookie), json!({}))
                .await;
            release.notify_one();
        }
    );
    assert_eq!(response.0, StatusCode::CONFLICT);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM linear_connections")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    f.lock().unwrap().mode = "normal".into();
    connect(&h, &cookie).await;
    let job = job(&h, &cookie).await;
    let host = Host::new(&h.state, &job).await.unwrap();
    f.lock().unwrap().mode = "pause".into();
    let (read, _) = tokio::join!(host.execute("linear_issue_list", json!({})), async {
        entered.notified().await;
        h.request("DELETE", "/api/linear/connection", Some(&cookie), json!({}))
            .await;
        release.notify_one();
    });
    assert_eq!(read["error"], "linear_connection_changed");
    assert!(read.get("items").is_none());
    drop(host);
    server.abort();
    h.close().await;
}

// 验证失效密钥禁用工具、权限不足仍可保留连接；只模拟供应商响应，不验证真实吊销传播。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn invalid_key_and_forbidden_are_distinct() {
    let (h, f, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let job = job(&h, &cookie).await;
    let host = Host::new(&h.state, &job).await.unwrap();
    f.lock().unwrap().mode = "forbidden".into();
    assert_eq!(
        host.execute("linear_issue_list", json!({})).await["error"],
        "linear_permission_denied"
    );
    assert!(
        Host::new(&h.state, &job)
            .await
            .unwrap()
            .catalog()
            .iter()
            .any(|tool| tool.provider == "linear")
    );
    f.lock().unwrap().mode = "invalid_key".into();
    assert_eq!(
        host.execute("linear_issue_list", json!({})).await["error"],
        "linear_invalid_key"
    );
    let unavailable = Host::new(&h.state, &job).await.unwrap();
    assert!(
        !unavailable
            .catalog()
            .iter()
            .any(|tool| tool.provider == "linear")
    );
    let (_, _, status) = h
        .request("GET", "/api/linear/status", Some(&cookie), Value::Null)
        .await;
    assert_eq!(status["connection"]["status"], "reauthorize");
    drop(unavailable);
    drop(host);
    server.abort();
    h.close().await;
}

// 验证更换密钥后必须重连，旧实例和旧工具不能继承新账号；不模拟多实例配置分发。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn key_rotation_fences_old_instances() {
    let (h, f, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let job = job(&h, &cookie).await;
    let host = Host::new(&h.state, &job).await.unwrap();
    let mut config = (*h.state.config).clone();
    config.linear.as_mut().unwrap().api_key = "fixture-rotated-key".into();
    let rotated = AppState::new(config, h.state.pool.clone()).unwrap();
    assert!(
        Host::new(&rotated, &job)
            .await
            .unwrap()
            .catalog()
            .iter()
            .all(|tool| tool.provider != "linear")
    );
    let app = router(rotated.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/api/linear/connection")
        .header("cookie", &cookie)
        .header("origin", ORIGIN)
        .body(Body::empty())
        .unwrap();
    assert_eq!(app.oneshot(request).await.unwrap().status(), StatusCode::OK);
    assert!(
        Host::new(&rotated, &job)
            .await
            .unwrap()
            .execute("linear_issue_list", json!({}))
            .await["items"]
            .is_array()
    );
    let before = f.lock().unwrap().requests.len();
    assert_eq!(
        host.execute("linear_issue_update", update_args()).await["error"],
        "linear_connection_changed"
    );
    assert_eq!(f.lock().unwrap().requests.len(), before);
    drop(host);
    server.abort();
    h.close().await;
}

// 验证缺失配置、错误工作空间、旧 OAuth 行及未登录访问不会启用工具；不访问真实 Linear。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn configuration_and_legacy_connection_boundaries() {
    let (mut h, f, server) = setup().await;
    let cookie = h.login().await;
    assert_eq!(
        h.request("POST", "/api/linear/connection", None, json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let mut config = (*h.state.config).clone();
    config.linear.as_mut().unwrap().workspace_slug = Some("another-workspace".into());
    h.state = AppState::new(config.clone(), h.state.pool.clone()).unwrap();
    h.app = router(h.state.clone());
    assert_eq!(
        h.request("POST", "/api/linear/connection", Some(&cookie), json!({}))
            .await
            .2["error"],
        "linear_workspace_mismatch"
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM linear_connections")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    config.linear.as_mut().unwrap().workspace_slug = None;
    h.state = AppState::new(config.clone(), h.state.pool.clone()).unwrap();
    h.app = router(h.state.clone());
    connect(&h, &cookie).await;
    // 模拟升级后保留的旧 OAuth 行，摘要为空时不能继承历史授权。
    sqlx::query("UPDATE linear_connections SET key_fingerprint=NULL,credentials=$1,expires_at=now(),scopes=ARRAY['read','write']")
        .bind(b"legacy-encrypted-token".to_vec()).execute(&h.state.pool).await.unwrap();
    let job = job(&h, &cookie).await;
    assert!(
        Host::new(&h.state, &job)
            .await
            .unwrap()
            .catalog()
            .iter()
            .all(|tool| tool.provider != "linear")
    );
    connect(&h, &cookie).await;
    let credentials: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT credentials FROM linear_connections")
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert!(credentials.is_none());
    config.linear = None;
    h.state = AppState::new(config, h.state.pool.clone()).unwrap();
    h.app = router(h.state.clone());
    let before = f.lock().unwrap().requests.len();
    let status = h
        .request("GET", "/api/linear/status", Some(&cookie), Value::Null)
        .await
        .2;
    assert_eq!(status["configured"], false);
    assert!(status["connection"].is_null());
    assert_eq!(
        h.request("POST", "/api/linear/connection", Some(&cookie), json!({}))
            .await
            .2["error"],
        "linear_disabled"
    );
    assert_eq!(f.lock().unwrap().requests.len(), before);
    server.abort();
    h.close().await;
}
