mod deployment_tests;
mod extraction_tests;
mod library_tests;
mod private_subscription_tests;
mod progress_tests;
mod removal_job_tests;
mod removal_tests;
mod retained_tests;
mod subscription_tests;
mod summary_tests;
mod tool_tests;
use super::*;
use axum::{
    extract::{Query, State},
    routing::get,
};
use orbit_server::communications;
use std::collections::HashMap;

/// 用明确的到达与释放信号控制单条读取，避免并发测试依赖固定休眠。
struct MessageGate {
    /// 上游已经捕获这次请求的响应快照。
    arrived: tokio::sync::Notify,
    /// 测试完成暂停或恢复后允许旧响应返回。
    release: tokio::sync::Notify,
}

/// 本地协议夹具的可控故障，不连接飞书租户或读取真实用户聊天。
struct Fixture {
    /// 第二页响应故障，用于验证断点重放。
    fail_second: bool,
    /// 延迟采集页，用来验证暂停的在途围栏。
    slow: bool,
    /// 已进入消息读取的次数。
    reads: usize,
    /// 单条消息的可选时序控制，仅供重处理在途测试。
    message_gate: Option<Arc<MessageGate>>,
    /// 修改已导入消息内容。
    edited: bool,
    /// 原文返回撤回墓碑。
    recalled: bool,
    /// 生成无效摘要证据。
    forged: bool,
    /// OAuth 刷新调用次数。
    refreshes: usize,
    /// 将第一页改为图片消息，验证下载和多模态派生流程。
    image: bool,
    /// 原图下载的 HTTP 状态，仅用于分类测试，不模拟实际租户权限。
    image_status: StatusCode,
    /// 仅私聊自动发现请求使用的分页夹具，其他手动查找保持原行为。
    private_pages: Vec<Value>,
    /// 捕获发现响应后暂停，验证移除及重连围栏。
    private_gate: Option<Arc<MessageGate>>,
    /// 同一天跨两页返回 60 条，用于证明不存在每日五条限制。
    bulk: bool,
    /// 群聊夹具用于核对个人关联范围。
    group: bool,
    /// 第二页明确提及本人。
    mention_me: bool,
    /// 第二页直接回复第一页本人消息。
    reply_to_me: bool,
    /// 第一页返回交互卡片的可见文字结构。
    card: bool,
    /// 自然日保持一致的测试时间.
    base: i64,
}
/// 为一个已有数据库夹具接入独立只读飞书协议服务器。
async fn setup() -> (Harness, Arc<Mutex<Fixture>>, tokio::task::JoinHandle<()>) {
    let mut h = memory::setup(true).await;
    let fixture = Arc::new(Mutex::new(Fixture {
        fail_second: false,
        slow: false,
        reads: 0,
        message_gate: None,
        edited: false,
        recalled: false,
        forged: false,
        refreshes: 0,
        image: false,
        image_status: StatusCode::OK,
        private_pages: vec![],
        private_gate: None,
        bulk: false,
        group: false,
        mention_me: false,
        reply_to_me: false,
        card: false,
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
    .route("/im/v1/chats",get(|State(fixture):State<Arc<Mutex<Fixture>>>,Query(query):Query<HashMap<String,String>>|async move {
        let (data, gate) = {
            let f = fixture.lock().unwrap();
            if query.get("types").is_some_and(|types| types == "p2p") && !f.private_pages.is_empty() {
                let page = query.get("page_token").and_then(|value| value.strip_prefix("page-")).and_then(|value| value.parse::<usize>().ok()).unwrap_or(0);
                (f.private_pages[page.min(f.private_pages.len()-1)].clone(), f.private_gate.clone())
            } else {
                (json!({"code":0,"data":{"items":[{"chat_id":"oc_fixture","name":"测试沟通","chat_mode":if f.group {"group"}else{"p2p"}}],"has_more":false,"page_token":""}}), None)
            }
        };
        if let Some(gate) = gate { gate.arrived.notify_one(); gate.release.notified().await; }
        Json(data)
    }))
    .route("/im/v1/chats/oc_fixture/members",get(|Query(query):Query<HashMap<String,String>>|async move {
        if query.get("page_token").is_some_and(|s|s=="members_second") {Json(json!({"code":0,"data":{"items":[{"member_id":"ou_other","name":"小林"}],"has_more":false}}))}
        else {Json(json!({"code":0,"data":{"items":[{"member_id":"ou_unrelated","name":"其他成员"}],"has_more":true,"page_token":"members_second"}}))}
    }))
    .route("/im/v1/messages/om_me",get(single_message))
    .route("/im/v1/messages/om_me/resources/img_fixture",get(|State(fixture):State<Arc<Mutex<Fixture>>>,headers:HeaderMap|async move {
        assert_eq!(headers["authorization"],"Bearer fixture-user-access");
        (fixture.lock().unwrap().image_status,[("content-type","application/octet-stream")],vec![137u8,80,78,71,13,10,26,10])
    }))
    .route("/im/v1/messages",get(|State(fixture):State<Arc<Mutex<Fixture>>>,headers:HeaderMap,Query(query):Query<HashMap<String,String>>|async move {
        assert_eq!(headers["authorization"],"Bearer fixture-user-access");
        let slow={let mut fixture=fixture.lock().unwrap();fixture.reads+=1;fixture.slow};
        if slow && query.get("page_size").is_some_and(|s|s=="50") {tokio::time::sleep(Duration::from_millis(400)).await;}
        let fixture=fixture.lock().unwrap();
        if query.get("page_size").is_some_and(|s| s == "1") && query.contains_key("start_time") {
            let start: i64 = query["start_time"].parse().unwrap();
            let end: i64 = query["end_time"].parse().unwrap();
            let active = fixture.base / 1000 >= start && fixture.base / 1000 < end;
            return Json(json!({"code":0,"data":{"items":if active {json!([{"message_id":"om_me"}])}else{json!([])},"has_more":false}}));
        }
        let second=query.get("page_token").is_some_and(|s|s=="second");
        if fixture.fail_second && second {return Json(json!({"code":999,"data":{}}));}
        if fixture.bulk {
            assert_eq!(query.get("page_size").map(String::as_str),Some("50"));
            let items:Vec<Value>=(if second {50..60} else {0..50}).map(|i|json!({"message_id":format!("om_bulk_{i}"),"chat_id":"oc_fixture","sender":{"id":if i==59 {"cli_fixture"} else {"ou_other"},"id_type":if i==59 {"app_id"} else {"open_id"},"sender_type":if i==59 {"app"} else {"user"}},"create_time":(fixture.base+i*1000).to_string(),"msg_type":"text","body":{"content":json!({"text":format!("记录 {i}")}).to_string()}})).collect();
            return Json(json!({"code":0,"data":{"items":items,"has_more":!second,"page_token":if second {""} else {"second"}}}));
        }
        let text=if second {"我来整理散步调研材料"} else if fixture.forged {"伪造证据：我来整理材料"} else if fixture.edited {"材料计划取消，先等反馈"} else {"我明天把材料发给你"};
        let deleted=fixture.recalled && !second;
        Json(json!({"code":0,"data":{"items":[{"message_id":if second {"om_other"} else {"om_me"},"chat_id":"oc_fixture","sender":{"id":if second {"ou_other"} else {"ou_allowed"},"id_type":"open_id","sender_type":"user"},"create_time":(fixture.base+if second {1000} else {0}).to_string(),"update_time":(fixture.base+if fixture.edited || fixture.recalled {2000} else {0}).to_string(),"mentions":if fixture.mention_me && second {json!([{"id":"ou_allowed","id_type":"open_id"}])}else{json!([])},"parent_id":if fixture.reply_to_me && second {"om_me"}else{""},"msg_type":if fixture.card && !second {"interactive"} else if fixture.image && !second {"image"} else {"text"},"deleted":deleted,"body":{"content":if fixture.card && !second {json!({"title":"项目进展","elements":[[{"tag":"text","text":"我明天把材料发给你"}]]}).to_string()} else if fixture.image && !second {json!({"image_key":"img_fixture"}).to_string()} else {json!({"text":text}).to_string()}}}],"has_more":!second,"page_token":if second {""} else {"second"}}}))
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
/// 返回可计数、可延迟的单条消息；响应在等待前捕获，模拟暂停前已发起的读取。
async fn single_message(State(fixture): State<Arc<Mutex<Fixture>>>) -> Json<Value> {
    let (value, gate) = {
        let mut fixture = fixture.lock().unwrap();
        fixture.reads += 1;
        let text = if fixture.edited {
            "材料计划取消，先等反馈"
        } else {
            "我明天把材料发给你"
        };
        let value = json!({"code":0,"data":{"items":[{
            "message_id":"om_me","chat_id":"oc_fixture",
            "sender":{"id":"ou_allowed","id_type":"open_id","sender_type":"user"},
            "create_time":fixture.base.to_string(),
            "update_time":(fixture.base+if fixture.edited {2000} else {0}).to_string(),
            "msg_type":"text","body":{"content":json!({"text":text}).to_string()}
        }]}});
        (value, fixture.message_gate.clone())
    };
    if let Some(gate) = gate {
        gate.arrived.notify_one();
        gate.release.notified().await;
    }
    Json(value)
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
            requests.iter().any(
                |request| request["tools"].as_array().is_some_and(|tools| tools
                    .iter()
                    .any(|tool| tool["function"]["name"] == "tools_search"))
            )
        );
        assert!(
            requests
                .iter()
                .any(|v| v["messages"]
                    .as_array()
                    .is_some_and(|messages| messages.iter().any(|m| m["content"]
                        .as_str()
                        .is_some_and(|s| s.contains("沟通资料检索结果")
                            && s.contains("小林")
                            && s.contains("source_url")
                            && !s.contains("om_other")
                            && !s.contains("ou_other")))))
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

// 验证无原文支持的候选被拒绝、合格候选保留为部分结果，原文仍可检索；不验证真实模型语义。
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
    communications::summary_jobs::step(&h.state).await.unwrap();
    let (state, hash): (String, Option<String>) =
        sqlx::query_as("SELECT summary_status,summary_hash FROM communication_documents LIMIT 1")
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert_eq!(state, "partial");
    assert!(hash.is_some());
    let hits = communications::retrieve(&h.state, "admin", "材料")
        .await
        .unwrap();
    assert!(!hits.is_empty());
    let summary = hits[0].summary.as_ref().unwrap();
    assert_eq!(summary.items.len(), 1);
    assert_eq!(summary.items[0].message_id, "om_other");
    assert_eq!(summary.rejected_count, 1);
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

/// 夹具覆盖群聊无关消息排除、显式提及与跨页直接回复；不代表真实飞书权限覆盖。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn group_scope_filters_and_keeps_mentions_and_direct_replies() {
    for mode in ["unrelated", "mention", "reply"] {
        let (h, fixture, server) = setup().await;
        {
            let mut f = fixture.lock().unwrap();
            f.group = true;
            f.mention_me = mode == "mention";
            f.reply_to_me = mode == "reply";
        }
        let cookie = h.login().await;
        connect(&h, &cookie).await;
        add(&h, &cookie).await;
        for _ in 0..2 {
            due(&h).await;
            communications::sync::step(&h.state).await.unwrap();
        }
        let id: Uuid = sqlx::query_scalar("SELECT id FROM communication_documents LIMIT 1")
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
        let (status, _, doc) = h
            .request(
                "GET",
                &format!("/api/communications/documents/{id}"),
                Some(&cookie),
                json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            doc["total"],
            if mode == "unrelated" { 1 } else { 2 },
            "{mode}: {doc}"
        );
        server.abort();
        h.close().await;
    }
}

/// 夹具验证卡片文字入库且旧版本被读取围栏阻挡；不评价模型对卡片的归纳质量。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn cards_are_extracted_and_old_scope_is_reprocessed() {
    let (h, fixture, server) = setup().await;
    fixture.lock().unwrap().card = true;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    add(&h, &cookie).await;
    due(&h).await;
    communications::sync::step(&h.state).await.unwrap();
    let id: Uuid = sqlx::query_scalar("SELECT id FROM communication_documents LIMIT 1")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    let (_, _, doc) = h
        .request(
            "GET",
            &format!("/api/communications/documents/{id}"),
            Some(&cookie),
            json!({}),
        )
        .await;
    assert!(
        doc["messages"][0]["text"]
            .as_str()
            .unwrap()
            .contains("项目进展")
    );
    sqlx::query("UPDATE communication_documents SET extraction_version=0")
        .execute(&h.state.pool)
        .await
        .unwrap();
    let (_, _, pending) = h
        .request(
            "GET",
            &format!("/api/communications/documents/{id}"),
            Some(&cookie),
            json!({}),
        )
        .await;
    assert_eq!(pending["processing"], true);
    assert_eq!(pending["total"], 0);
    let (status, _, hits) = h
        .request(
            "GET",
            "/api/communications/search?q=material",
            Some(&cookie),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(hits.as_array().unwrap().is_empty());
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(communications::sync::run(h.state.clone(), receiver));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let ready: bool = sqlx::query_scalar(
                "SELECT extraction_version=1 FROM communication_documents WHERE id=$1",
            )
            .bind(id)
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
            if ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    stop.send(true).unwrap();
    worker.await.unwrap();
    server.abort();
    h.close().await;
}

/// 显式推进持久化删除队列；不等待生产调度间隔，结果仍由真实状态接口读取。
async fn finish_removal(h: &Harness, cookie: &str) -> Value {
    for _ in 0..1010 {
        if !communications::removal_jobs::step(&h.state).await.unwrap() {
            let (_, _, status) = h
                .request(
                    "GET",
                    "/api/communications/status",
                    Some(cookie),
                    Value::Null,
                )
                .await;
            return status["removals"][0].clone();
        }
    }
    panic!("测试删除队列没有收敛");
}
