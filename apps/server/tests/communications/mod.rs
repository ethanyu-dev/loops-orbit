use super::*;
use axum::{
    extract::{Query, State},
    routing::get,
};
use orbit_server::communications;
use std::collections::HashMap;

/// 本地协议夹具的可控故障，不连接飞书租户或读取真实用户聊天。
struct Fixture {
    /// 第二页响应故障，用于验证断点重放。
    fail_second: bool,
    /// 延迟采集页，用来验证暂停的在途围栏。
    slow: bool,
    /// 已进入消息读取的次数。
    reads: usize,
    /// 修改已导入消息内容。
    edited: bool,
    /// 原文返回撤回墓碑。
    recalled: bool,
    /// 生成无效摘要证据。
    forged: bool,
    /// OAuth 刷新调用次数。
    refreshes: usize,
    /// 自然日保持一致的测试时间。
    base: i64,
}
/// 为一个已有数据库夹具接入独立只读飞书协议服务器。
async fn setup() -> (Harness, Arc<Mutex<Fixture>>, tokio::task::JoinHandle<()>) {
    let mut h = memory::setup(true).await;
    let fixture = Arc::new(Mutex::new(Fixture {
        fail_second: false,
        slow: false,
        reads: 0,
        edited: false,
        recalled: false,
        forged: false,
        refreshes: 0,
        base: chrono::Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis()
            - 86400000
            + 3600000,
    }));
    let app=Router::new().route("/authen/v2/oauth/token",post(|State(fixture):State<Arc<Mutex<Fixture>>>,Json(body):Json<Value>|async move {
        if body["grant_type"]=="refresh_token" {fixture.lock().unwrap().refreshes+=1;}
        Json(json!({"code":0,"access_token":"fixture-user-access","refresh_token":"fixture-user-refresh","expires_in":7200,"refresh_token_expires_in":86400}))
    })).route("/authen/v1/user_info",get(||async {Json(json!({"code":0,"data":{"open_id":"ou_allowed","name":"本地测试账号"}}))}))
    .route("/im/v1/chats",get(||async {Json(json!({"code":0,"data":{"items":[{"chat_id":"oc_fixture","name":"测试沟通","chat_mode":"p2p"}],"has_more":false,"page_token":""}}))}))
    .route("/im/v1/messages",get(|State(fixture):State<Arc<Mutex<Fixture>>>,headers:HeaderMap,Query(query):Query<HashMap<String,String>>|async move {
        assert_eq!(headers["authorization"],"Bearer fixture-user-access");
        let slow={let mut fixture=fixture.lock().unwrap();fixture.reads+=1;fixture.slow};
        if slow && query.get("page_size").is_some_and(|s|s=="50") {tokio::time::sleep(Duration::from_millis(400)).await;}
        let fixture=fixture.lock().unwrap();
        let second=query.get("page_token").is_some_and(|s|s=="second");
        if fixture.fail_second && second {return Json(json!({"code":999,"data":{}}));}
        let text=if second {"我来整理散步调研材料"} else if fixture.forged {"伪造证据：我来整理材料"} else if fixture.edited {"材料计划取消，先等反馈"} else {"我明天把材料发给你"};
        let deleted=fixture.recalled && !second;
        Json(json!({"code":0,"data":{"items":[{"message_id":if second {"om_other"} else {"om_me"},"chat_id":"oc_fixture","sender":{"id":if second {"ou_other"} else {"ou_allowed"},"id_type":"open_id","sender_type":"user"},"create_time":(fixture.base+if second {1000} else {0}).to_string(),"update_time":(fixture.base+if fixture.edited || fixture.recalled {2000} else {0}).to_string(),"msg_type":"text","deleted":deleted,"body":{"content":json!({"text":text}).to_string()}}],"has_more":!second,"page_token":if second {""} else {"second"}}}))
    })).with_state(fixture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut config = (*h.state.config).clone();
    config.communications = Some(communications::Config { token_key: [7; 32] });
    config.feishu.as_mut().unwrap().api_base = base;
    h.state = AppState::new(config, h.state.pool.clone()).unwrap();
    h.app = router(h.state.clone());
    (h, fixture, server)
}
/// 通过真实 OAuth 路由登录，模拟跨站回调只携带专用 Lax Cookie。
async fn start(h: &Harness, cookie: &str) -> (String, String) {
    let (status, oauth, value) = h
        .request(
            "POST",
            "/api/communications/oauth/start",
            Some(cookie),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let url = reqwest::Url::parse(value["url"].as_str().unwrap()).unwrap();
    (
        oauth.unwrap(),
        url.query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .into_owned(),
    )
}
/// 回调只访问本地令牌与用户信息夹具，未实际授予飞书租户权限。
async fn connect(h: &Harness, cookie: &str) {
    let (oauth, state) = start(h, cookie).await;
    let (status, _, _) = h
        .request(
            "GET",
            &format!("/api/communications/oauth/callback?state={state}&code=fixture-code"),
            Some(&oauth),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM communication_connections")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}
/// 明确选择一个会话后才允许后台采集。
async fn add(h: &Harness, cookie: &str) -> Uuid {
    let (status, _, value) = h
        .request(
            "POST",
            "/api/communications/sources",
            Some(cookie),
            json!({"chat_id":"oc_fixture","label":"测试沟通","days":7}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    Uuid::parse_str(value["id"].as_str().unwrap()).unwrap()
}
/// 推进测试数据库调度时间，不依赖真实十分钟等待。
async fn due(h: &Harness) {
    sqlx::query("UPDATE communication_sources SET next_sync=now()")
        .execute(&h.state.pool)
        .await
        .unwrap();
}
/// 完整导入两个分页，并等待真实整理和向量 worker 完成。
async fn import(h: &Harness, cookie: &str) -> Value {
    for _ in 0..2 {
        due(h).await;
        communications::sync::step(&h.state).await.unwrap();
    }
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(communications::sync::run(h.state.clone(), receiver));
    tokio::time::timeout(Duration::from_secs(15),async {
        loop {
            let ready:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM communication_documents WHERE summary_hash IS NOT NULL) AND EXISTS(SELECT 1 FROM memory_vectors WHERE owner='communications:admin')").fetch_one(&h.state.pool).await.unwrap();
            if ready {break;}tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }).await.unwrap();
    stop.send(true).unwrap();
    worker.await.unwrap();
    let (status, _, data) = h
        .request(
            "GET",
            "/api/communications/status",
            Some(cookie),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{data}");
    data["documents"][0].clone()
}

// 验证 OAuth 浏览器绑定、一次消费、注销失效和凭证加密；不覆盖真实授权页及租户审批。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn oauth_binding_replay_and_encrypted_refresh() {
    let (h, fixture, server) = setup().await;
    let cookie = h.login().await;
    let (oauth, state) = start(&h, &cookie).await;
    let callback = format!("/api/communications/oauth/callback?state={state}&code=fixture");
    assert_eq!(
        h.request("GET", &callback, None, Value::Null).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        h.request(
            "GET",
            &callback,
            Some("orbit_feishu_oauth=wrong"),
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        h.request("GET", &callback, Some(&oauth), Value::Null)
            .await
            .0,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        h.request("GET", &callback, Some(&oauth), Value::Null)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let bytes: Vec<u8> = sqlx::query_scalar("SELECT credentials FROM communication_connections")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("fixture-user"));
    sqlx::query("UPDATE communication_connections SET expires_at=now()")
        .execute(&h.state.pool)
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        h.request(
            "GET",
            "/api/communications/chats",
            Some(&cookie),
            Value::Null
        ),
        h.request(
            "GET",
            "/api/communications/chats",
            Some(&cookie),
            Value::Null
        )
    );
    assert_eq!(a.0, StatusCode::OK);
    assert_eq!(b.0, StatusCode::OK);
    assert_eq!(fixture.lock().unwrap().refreshes, 1);
    let (oauth, state) = start(&h, &cookie).await;
    h.request("POST", "/api/auth/logout", Some(&cookie), json!({}))
        .await;
    assert_eq!(
        h.request(
            "GET",
            &format!("/api/communications/oauth/callback?state={state}&code=fixture"),
            Some(&oauth),
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    server.abort();
    h.close().await;
}

// 验证两页中断不推进水位、重放去重和无机器人回复副作用；消息来自本地协议夹具。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn pagination_recovers_without_duplicate_messages_or_runs() {
    let (h, fixture, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    add(&h, &cookie).await;
    fixture.lock().unwrap().fail_second = true;
    for _ in 0..2 {
        due(&h).await;
        communications::sync::step(&h.state).await.unwrap();
    }
    let (watermark, start, error): (i64, i64, Option<String>) =
        sqlx::query_as("SELECT watermark,start_at,error FROM communication_sources")
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert_eq!(watermark, start);
    assert!(error.is_some());
    fixture.lock().unwrap().fail_second = false;
    let doc = import(&h, &cookie).await;
    let (_, _, detail) = h
        .request(
            "GET",
            &format!(
                "/api/communications/documents/{}",
                doc["id"].as_str().unwrap()
            ),
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(detail["total"], 2);
    assert_eq!(detail["messages"][0]["is_me"], true);
    assert_eq!(detail["messages"][1]["is_me"], false);
    assert_eq!(detail["summary"]["items"][0]["kind"], "my_commitment");
    assert_eq!(detail["summary"]["items"][1]["kind"], "their_commitment");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM runs")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM outbox")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    server.abort();
    h.close().await;
}

// 验证 BM25、向量回退、本人机器人上下文及访客隔离；三维夹具不代表语义效果验收。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn retrieval_is_scoped_and_used_as_external_evidence() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    add(&h, &cookie).await;
    import(&h, &cookie).await;
    let (_, _, hits) = h
        .request(
            "GET",
            "/api/communications/search?q=%E6%9D%90%E6%96%99",
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert!(!hits.as_array().unwrap().is_empty());
    let (_, _, hits) = h
        .request(
            "GET",
            "/api/communications/search?q=%E8%B5%B0%E4%B8%80%E8%B5%B0",
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert!(!hits.as_array().unwrap().is_empty());
    let conv = h.conversation(&cookie).await;
    assert!(
        !communications::retrieve(&h.state, "feishu:ou_allowed", "材料")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        communications::retrieve(&h.state, "feishu:ou_other", "材料")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        communications::retrieve(&h.state, "guest:someone", "材料")
            .await
            .unwrap()
            .is_empty()
    );
    memory::complete(&h, &cookie, conv, "材料怎么安排的？").await;
    {
        let requests = h.requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .any(|v| v["messages"]
                    .as_array()
                    .is_some_and(|messages| messages.iter().any(|m| m["content"]
                        .as_str()
                        .is_some_and(
                            |s| s.contains("沟通资料检索结果") && s.contains("om_other")
                        ))))
        );
    }
    let (_, _, link) = h
        .request(
            "POST",
            "/api/admin/links",
            Some(&cookie),
            json!({"label":"访客","expires_in_seconds":3600}),
        )
        .await;
    let token = link["url"]
        .as_str()
        .unwrap()
        .split("token=")
        .nth(1)
        .unwrap();
    let (_, guest, _) = h
        .request("POST", "/api/auth/exchange", None, json!({"token":token}))
        .await;
    assert_eq!(
        h.request(
            "GET",
            "/api/communications/search?q=%E6%9D%90%E6%96%99",
            guest.as_deref(),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    server.abort();
    h.close().await;
}

// 验证显式提醒绑定、编辑/撤回失效和断开清理；不向真实飞书接收人发送消息。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn corrected_evidence_cancels_followups_and_disconnect_forgets() {
    let (h, fixture, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    add(&h, &cookie).await;
    let doc = import(&h, &cookie).await;
    let body = json!({"idempotency_key":Uuid::new_v4(),"kind":"reminder","topic":"核对材料进度","due_at":(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339(),"communication":{"document_id":doc["id"],"version":doc["version"],"item":0}});
    let (status, _, task) = h
        .request("POST", "/api/followups", Some(&cookie), body)
        .await;
    assert_eq!(status, StatusCode::OK, "{task}");
    fixture.lock().unwrap().edited = true;
    // 强制全范围复查，模拟旧消息编辑由每日审计发现。
    sqlx::query(
        "UPDATE communication_sources SET audit_at=now()-interval '1 second',next_sync=now()",
    )
    .execute(&h.state.pool)
    .await
    .unwrap();
    communications::sync::step(&h.state).await.unwrap();
    let task_status: String = sqlx::query_scalar("SELECT status FROM followups WHERE id=$1")
        .bind(Uuid::parse_str(task["id"].as_str().unwrap()).unwrap())
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(task_status, "cancelled");
    let (_, _, detail) = h
        .request(
            "GET",
            &format!(
                "/api/communications/documents/{}",
                doc["id"].as_str().unwrap()
            ),
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert!(detail["summary"].is_null());
    assert!(
        detail["messages"][0]["text"]
            .as_str()
            .unwrap()
            .contains("取消")
    );
    fixture.lock().unwrap().recalled = true;
    sqlx::query("UPDATE communication_sources SET window_start=NULL,window_end=NULL,page_token='',audit_at=now()-interval '1 second',next_sync=now()").execute(&h.state.pool).await.unwrap();
    communications::sync::step(&h.state).await.unwrap();
    let (_, _, detail) = h
        .request(
            "GET",
            &format!(
                "/api/communications/documents/{}",
                doc["id"].as_str().unwrap()
            ),
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(detail["messages"][0]["deleted"], true);
    assert_eq!(detail["messages"][0]["text"], "");
    assert_eq!(
        h.request(
            "DELETE",
            "/api/communications/connection",
            Some(&cookie),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM communication_documents")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM memory_vectors WHERE owner='communications:admin'",
    )
    .fetch_one(&h.state.pool)
    .await
    .unwrap();
    assert_eq!(count, 0);
    let directory = h
        .state
        .config
        .memory
        .as_ref()
        .unwrap()
        .directory
        .join("_communications");
    assert_eq!(std::fs::read_dir(directory).unwrap().count(), 0);
    server.abort();
    h.close().await;
}

// 验证暂停能阻止已发起请求的晚到数据提交；延迟来自本地夹具，不测真实网络时序。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn pausing_source_fences_inflight_sync() {
    let (h, fixture, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    {
        let mut f = fixture.lock().unwrap();
        f.slow = true;
        f.reads = 0;
    }
    let state = h.state.clone();
    let sync = tokio::spawn(async move { communications::sync::step(&state).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if fixture.lock().unwrap().reads > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (status, _, body) = h
        .request(
            "PUT",
            &format!("/api/communications/sources/{source}"),
            Some(&cookie),
            json!({"version":1,"enabled":false}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    sync.await.unwrap().unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM communication_documents")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    server.abort();
    h.close().await;
}

// 验证无原文支持的模型候选不落库，原文仍能检索；不声称能验证自然语言语义蕴含。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn fabricated_summary_evidence_is_rejected() {
    let (h, fixture, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    add(&h, &cookie).await;
    fixture.lock().unwrap().forged = true;
    for _ in 0..2 {
        due(&h).await;
        communications::sync::step(&h.state).await.unwrap();
    }
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(communications::sync::run(h.state.clone(), receiver));
    tokio::time::timeout(Duration::from_secs(5),async {loop {
        let failed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM communication_documents WHERE summary_error IS NOT NULL AND summary_hash IS NULL)").fetch_one(&h.state.pool).await.unwrap();
        if failed {break;}tokio::time::sleep(Duration::from_millis(20)).await;
    }}).await.unwrap();
    stop.send(true).unwrap();
    worker.await.unwrap();
    assert!(
        !communications::retrieve(&h.state, "admin", "材料")
            .await
            .unwrap()
            .is_empty()
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM memory_vectors WHERE owner='communications:admin'",
    )
    .fetch_one(&h.state.pool)
    .await
    .unwrap();
    assert_eq!(count, 0);
    server.abort();
    h.close().await;
}

// 验证密文被篡改时拒绝使用，且接口不返回令牌；不替代 AEAD 库的密码学测试。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn tampered_credentials_fail_closed() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    sqlx::query("UPDATE communication_connections SET credentials=set_byte(credentials,20,(get_byte(credentials,20)+1)%256)").execute(&h.state.pool).await.unwrap();
    let (status, _, value) = h
        .request(
            "GET",
            "/api/communications/chats",
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(value["error"], "communication_unavailable");
    assert!(!value.to_string().contains("fixture-user"));
    server.abort();
    h.close().await;
}
