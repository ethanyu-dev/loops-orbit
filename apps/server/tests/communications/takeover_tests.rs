use super::*;
use orbit_server::communications::takeover;

/// 控制接管协议各阶段的夹具；不访问真实 Jev、聊天模型或飞书。
struct TakeoverFixture {
    /// 被采集的原始消息。
    message: Value,
    /// 使用较早的本人消息提供沟通知识，覆盖文档版本复核。
    communication_knowledge: bool,
    /// 匹配概率与复核概率独立，验证两道关口。
    match_probability: f64,
    /// 知识支持概率。
    review_probability: f64,
    /// 让起草器明确返回无法回答。
    unanswerable: bool,
    /// 最新消息检查时模拟本人已经介入。
    owner_replied: bool,
    /// 飞书实际授予发送权限。
    authorized: bool,
    /// 群聊不会进入接管队列。
    group: bool,
    /// 模拟发送请求已收到但返回失败，验证不自动重复发送。
    reject_send: bool,
    /// 复核阶段暂停，模拟生成期间规则或证据被修改。
    review_gate: Option<Arc<MessageGate>>,
    /// 捕获发送内容，不包含任何真实用户信息。
    sent: Vec<Value>,
    /// 捕获 Jev 请求形状。
    decisions: Vec<Value>,
}
/// 用生产 OAuth、采集、知识库和队列组装完整链路。
async fn setup_takeover() -> (
    Harness,
    Arc<Mutex<TakeoverFixture>>,
    tokio::task::JoinHandle<()>,
) {
    let mut h = memory::setup(false).await;
    let at = chrono::Utc::now().timestamp_millis() - 4000;
    let fixture = Arc::new(Mutex::new(TakeoverFixture {
        message: json!({"message_id":"om_question","chat_id":"oc_fixture","sender":{"id":"ou_other","id_type":"open_id","sender_type":"user"},"create_time":at.to_string(),"update_time":at.to_string(),"msg_type":"text","body":{"content":json!({"text":"怎么申请 novita 测试环境权限？"}).to_string()}}),
        communication_knowledge: false,
        match_probability: 0.98,
        review_probability: 0.98,
        unanswerable: false,
        owner_replied: false,
        authorized: true,
        group: false,
        reject_send: false,
        review_gate: None,
        sent: vec![],
        decisions: vec![],
    }));
    let app = Router::new()
        .route("/authen/v2/oauth/token", post(|State(f):State<Arc<Mutex<TakeoverFixture>>>|async move {
            let scope = if f.lock().unwrap().authorized { "im:message im:message.send_as_user offline_access" } else { "im:message:readonly offline_access" };
            Json(json!({"access_token":"fixture-personal-token","refresh_token":"fixture-refresh","expires_in":7200,"refresh_token_expires_in":86400,"scope":scope}))
        }))
        .route("/authen/v1/user_info", get(|| async { Json(json!({"code":0,"data":{"open_id":"ou_allowed","name":"测试本人"}})) }))
        .route("/im/v1/chats", get(|State(f):State<Arc<Mutex<TakeoverFixture>>>| async move { Json(json!({"code":0,"data":{"items":[{"chat_id":"oc_fixture","name":"测试联系人","chat_mode":if f.lock().unwrap().group {"group"} else {"p2p"}}],"has_more":false}})) }))
        .route("/im/v1/chats/oc_fixture/members",get(||async {Json(json!({"code":0,"data":{"items":[],"has_more":false}}))}))
        .route("/im/v1/messages", get(|State(f):State<Arc<Mutex<TakeoverFixture>>>,headers:HeaderMap,Query(query):Query<HashMap<String,String>>|async move {
            assert_eq!(headers["authorization"],"Bearer fixture-personal-token");
            let f=f.lock().unwrap();
            let mut items=vec![f.message.clone()];
            if f.communication_knowledge {
                let mut knowledge = f.message.clone();
                knowledge["message_id"] = json!("om_knowledge");
                knowledge["sender"]["id"] = json!("ou_allowed");
                let at = f.message["create_time"].as_str().unwrap().parse::<i64>().unwrap() - 1000;
                knowledge["create_time"] = json!(at.to_string());
                knowledge["update_time"] = json!(at.to_string());
                knowledge["body"]["content"] = json!(json!({"text":"申请 novita 测试环境权限：提交申请表，填写测试用途，由环境管理员审核。"}).to_string());
                items.push(knowledge);
            }
            if f.owner_replied && query.get("sort_type").is_some_and(|v|v=="ByCreateTimeDesc") {
                let mut own=f.message.clone();own["message_id"]=json!("om_owner_reply");own["sender"]["id"]=json!("ou_allowed");own["create_time"]=json!(chrono::Utc::now().timestamp_millis().to_string());items.push(own);
            }
            Json(json!({"code":0,"data":{"items":items,"has_more":false}}))
        }))
        .route("/im/v1/messages/om_question/reply",post(|State(f):State<Arc<Mutex<TakeoverFixture>>>,headers:HeaderMap,Json(body):Json<Value>|async move {
            assert_eq!(headers["authorization"],"Bearer fixture-personal-token");
            let mut f=f.lock().unwrap();f.sent.push(body);
            Json(json!({"code":if f.reject_send {999} else {0},"data":{"message_id":"om_agent_reply"}}))
        }))
        .route("/v1/systemone",post(|State(f):State<Arc<Mutex<TakeoverFixture>>>,headers:HeaderMap,Json(body):Json<Value>|async move {
            assert_eq!(headers["authorization"],"Bearer fixture-typesafe-key");assert_eq!(body["model"],"jev-latest");
            let (answers, gate) = {
                let mut f=f.lock().unwrap(); f.decisions.push(body.clone());
                let answers:serde_json::Map<String,Value>=body["questions"].as_object().unwrap().iter().map(|(key,value)| {
                    assert_eq!(value["type"],"noul");
                    (key.clone(),json!({"type":"noul","noul":if key=="answerable" {f.review_probability}else{f.match_probability}}))
                }).collect();
                (answers, if body["questions"].get("answerable").is_some() {f.review_gate.clone()} else {None})
            };
            if let Some(gate) = gate { gate.arrived.notify_one(); gate.release.notified().await; }
            Json(json!({"model":"jev-fixture","answers":answers,"usage":{"input_tokens":1,"output_tokens":0}}))
        }))
        .route("/v1/chat/completions",post(|State(f):State<Arc<Mutex<TakeoverFixture>>>,Json(body):Json<Value>|async move {
            assert!(body.get("tools").is_none());
            assert!(body["messages"][0]["content"].as_str().unwrap().contains("知识问答起草器"));
            let input:Value=serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
            let evidence=input["evidence"].as_array().unwrap().iter().find(|e|e["text"].as_str().unwrap().contains("提交申请表"));
            let answer = match evidence.filter(|_| !f.lock().unwrap().unanswerable) {
                Some(evidence) => json!({"answer":"请提交申请表，填写测试用途，由环境管理员审核。","citations":[{"id":evidence["id"],"quote":"提交申请表，填写测试用途"}]}),
                None => json!({"answer":null,"citations":[]}),
            };
            Json(json!({"choices":[{"message":{"content":answer.to_string()}}]}))
        })).with_state(fixture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut config = (*h.state.config).clone();
    config.takeover_questions_file = config
        .memory
        .as_ref()
        .unwrap()
        .directory
        .join("takeover-questions.json");
    std::fs::write(
        &config.takeover_questions_file,
        include_str!("../../../../config/takeover-questions.json"),
    )
    .unwrap();
    config.communications = Some(communications::Config { token_key: [7; 32] });
    config.feishu.as_mut().unwrap().api_base = base.clone();
    config.typesafe = Some(takeover::Config {
        base_url: base.clone(),
        api_key: "fixture-typesafe-key".into(),
        model: "jev-latest".into(),
    });
    config.model.base_url = format!("{base}/v1");
    config.model.stream_enabled = false;
    h.state = AppState::new(config, h.state.pool.clone()).unwrap();
    h.app = router(h.state.clone());
    (h, fixture, server)
}
/// 规则仍走真实管理接口，仅将时间边界移到夹具消息之前以避免固定睡眠。
async fn enable(h: &Harness, cookie: &str) {
    let settings = snapshot(h, cookie).await;
    let (status,_,value)=h.request("PUT","/api/communications/takeover",Some(cookie),json!({"enabled":true,"threshold":0.9,"version":settings["version"],"rules_revision":settings["rules_revision"]})).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    sqlx::query("UPDATE communication_takeover_settings SET since_ms=$1")
        .bind(chrono::Utc::now().timestamp_millis() - 10_000)
        .execute(&h.state.pool)
        .await
        .unwrap();
}
/// 从管理接口读取当前版本，不假定问题文件的摘要值。
async fn snapshot(h: &Harness, cookie: &str) -> Value {
    let (status, _, value) = h
        .request(
            "GET",
            "/api/communications/takeover",
            Some(cookie),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value["settings"].clone()
}
/// 仅修改该测试隔离目录内的文件，不能污染仓库中的默认问题。
fn write_questions(h: &Harness, document: Value) {
    std::fs::write(
        &h.state.config.takeover_questions_file,
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
}
/// 本人通过管理接口明确发布流程，私人记忆不再自动进入代答证据。
async fn knowledge(h: &Harness, cookie: &str) {
    let (status,_,body)=h.request("POST","/api/knowledge",Some(cookie),json!({"title":"novita 测试环境申请流程","content":"申请 novita 测试环境权限的流程：提交申请表，填写测试用途，由环境管理员审核。","status":"published"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

// 验证用户授权、采集入队、Jev 两阶段、知识出处、标识及重放去重；不验收真实模型语义质量或租户投递。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn takeover_sends_once_as_user_with_agent_marker() {
    let (h, f, server) = setup_takeover().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    add(&h, &cookie).await;
    knowledge(&h, &cookie).await;
    enable(&h, &cookie).await;
    communications::sync::step(&h.state).await.unwrap();
    takeover::step(&h.state).await.unwrap();
    let status: String = sqlx::query_scalar("SELECT status FROM communication_takeover_jobs")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(status, "sent");
    due(&h).await;
    communications::sync::step(&h.state).await.unwrap();
    takeover::step(&h.state).await.unwrap();
    {
        let f = f.lock().unwrap();
        assert_eq!(f.sent.len(), 1);
        assert_eq!(f.decisions.len(), 2);
        let content: Value = serde_json::from_str(f.sent[0]["content"].as_str().unwrap()).unwrap();
        assert!(content["text"].as_str().unwrap().starts_with("[agent] "));
        assert!(f.sent[0]["uuid"].as_str().is_some());
    }
    server.abort();
    h.close().await;
}

// 验证各阶段保持静默及未知投递不重发；夹具概率是指定值，不代表真实 Jev 准确率。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn takeover_silences_unmatched_unsupported_changed_and_uncertain_messages() {
    for scenario in [
        "unmatched",
        "unanswerable",
        "unsupported",
        "no_knowledge",
        "owner_replied",
        "disabled",
        "unknown",
    ] {
        let (h, f, server) = setup_takeover().await;
        let cookie = h.login().await;
        connect(&h, &cookie).await;
        add(&h, &cookie).await;
        if scenario != "no_knowledge" {
            knowledge(&h, &cookie).await;
        }
        enable(&h, &cookie).await;
        {
            let mut f = f.lock().unwrap();
            match scenario {
                "unmatched" => f.match_probability = 0.5,
                "unanswerable" => f.unanswerable = true,
                "unsupported" => f.review_probability = 0.5,
                "owner_replied" => f.owner_replied = true,
                "unknown" => f.reject_send = true,
                _ => {}
            }
        }
        communications::sync::step(&h.state).await.unwrap();
        if scenario == "disabled" {
            let (status, _, _) = h
                .request(
                    "PUT",
                    "/api/communications/takeover",
                    Some(&cookie),
                    json!({"enabled":false,"threshold":0.9,"version":1}),
                )
                .await;
            assert_eq!(status, StatusCode::OK);
        }
        takeover::step(&h.state).await.unwrap();
        takeover::step(&h.state).await.unwrap();
        let status: String = sqlx::query_scalar("SELECT status FROM communication_takeover_jobs")
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
        assert_eq!(
            status,
            if scenario == "unknown" {
                "unknown"
            } else {
                "ignored"
            },
            "{scenario}"
        );
        assert_eq!(
            f.lock().unwrap().sent.len(),
            usize::from(scenario == "unknown"),
            "{scenario}"
        );
        server.abort();
        h.close().await;
    }
}

// 验证管理认证、真实 scope 缺失、并发版本和本人/历史/群聊隔离，不覆盖飞书后台审批。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn takeover_requires_authorization_and_excludes_non_new_private_messages() {
    let (h, f, server) = setup_takeover().await;
    let cookie = h.login().await;
    assert_eq!(
        h.request("GET", "/api/communications/takeover", None, Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    f.lock().unwrap().authorized = false;
    connect(&h, &cookie).await;
    let settings = snapshot(&h, &cookie).await;
    let body = json!({"enabled":true,"threshold":0.9,"version":0,"rules_revision":settings["rules_revision"]});
    assert_eq!(
        h.request(
            "PUT",
            "/api/communications/takeover",
            Some(&cookie),
            body.clone()
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    f.lock().unwrap().authorized = true;
    connect(&h, &cookie).await;
    let id = add(&h, &cookie).await;
    enable(&h, &cookie).await;
    assert_eq!(
        h.request("PUT", "/api/communications/takeover", Some(&cookie), body)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let today = chrono::Utc::now()
        .with_timezone(&chrono_tz::Asia::Shanghai)
        .date_naive()
        .to_string();
    assert_eq!(
        h.request(
            "POST",
            &format!("/api/communications/sources/{id}/history"),
            Some(&cookie),
            json!({"start_date":today,"end_date":today})
        )
        .await
        .0,
        StatusCode::OK
    );
    communications::subscription::history_step(&h.state)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM communication_takeover_jobs")
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        0
    );
    f.lock().unwrap().message["sender"]["id"] = json!("ou_allowed");
    communications::sync::step(&h.state).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM communication_takeover_jobs")
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        0
    );
    f.lock().unwrap().message["sender"]["id"] = json!("ou_other");
    sqlx::query("UPDATE communication_sources SET chat_mode='group',next_sync=now() WHERE id=$1")
        .bind(id)
        .execute(&h.state.pool)
        .await
        .unwrap();
    communications::sync::step(&h.state).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM communication_takeover_jobs")
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        0
    );
    server.abort();
    h.close().await;
}

// 验证模型复核期间改规则或删除知识不会发送旧答案；不模拟平台发送开始后的撤回。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn takeover_rechecks_inflight_settings_and_knowledge() {
    for scenario in [
        "disable",
        "forget",
        "edited",
        "revoked",
        "file_changed",
        "file_invalid",
        "file_missing",
    ] {
        let (h, f, server) = setup_takeover().await;
        let cookie = h.login().await;
        connect(&h, &cookie).await;
        add(&h, &cookie).await;
        knowledge(&h, &cookie).await;
        enable(&h, &cookie).await;
        let gate = Arc::new(MessageGate {
            arrived: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        f.lock().unwrap().review_gate = Some(gate.clone());
        communications::sync::step(&h.state).await.unwrap();
        let state = h.state.clone();
        let work = tokio::spawn(async move { takeover::step(&state).await.unwrap() });
        tokio::time::timeout(Duration::from_secs(5), gate.arrived.notified())
            .await
            .unwrap();
        match scenario {
            "disable" => {
                assert_eq!(
                    h.request(
                        "PUT",
                        "/api/communications/takeover",
                        Some(&cookie),
                        json!({"enabled":false,"threshold":0.9,"version":1})
                    )
                    .await
                    .0,
                    StatusCode::OK
                );
            }
            "forget" => {
                let (_, _, entries) = h
                    .request("GET", "/api/knowledge", Some(&cookie), Value::Null)
                    .await;
                assert_eq!(
                    h.request(
                        "DELETE",
                        &format!(
                            "/api/knowledge/{}",
                            entries["items"][0]["id"].as_str().unwrap()
                        ),
                        Some(&cookie),
                        Value::Null
                    )
                    .await
                    .0,
                    StatusCode::OK
                );
            }
            "edited" => {
                f.lock().unwrap().message["body"]["content"] =
                    json!(json!({"text":"不用申请了"}).to_string());
            }
            "file_changed" => {
                write_questions(&h, json!({"questions":["查询 novita 测试环境使用流程"]}));
            }
            "file_invalid" => {
                std::fs::write(&h.state.config.takeover_questions_file, "{broken").unwrap();
            }
            "file_missing" => {
                std::fs::remove_file(&h.state.config.takeover_questions_file).unwrap();
            }
            "revoked" => {
                f.lock().unwrap().authorized = false;
                sqlx::query(
                    "UPDATE communication_connections SET expires_at=now()-interval '1 minute'",
                )
                .execute(&h.state.pool)
                .await
                .unwrap();
            }
            _ => unreachable!(),
        }
        gate.release.notify_one();
        work.await.unwrap();
        assert!(f.lock().unwrap().sent.is_empty(), "{scenario}");
        server.abort();
        h.close().await;
    }
}

// 验证文件作为唯一来源、多问题传入 Jev、排版不作废以及 API 不能覆写问题；不验证真实 Jev 区分多个意图的质量。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn takeover_loads_multiple_questions_from_file_and_rejects_api_overrides() {
    let (h, f, server) = setup_takeover().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    add(&h, &cookie).await;
    let first = snapshot(&h, &cookie).await;
    let document = json!({"questions":["申请 novita 测试环境权限", "查询测试环境使用流程"]});
    write_questions(&h, document.clone());
    let stale = json!({"enabled":true,"threshold":0.9,"version":first["version"],"rules_revision":first["rules_revision"]});
    assert_eq!(
        h.request("PUT", "/api/communications/takeover", Some(&cookie), stale)
            .await
            .0,
        StatusCode::CONFLICT
    );
    enable(&h, &cookie).await;
    let before = snapshot(&h, &cookie).await;
    assert_eq!(before["topics"], document["questions"]);
    std::fs::write(
        &h.state.config.takeover_questions_file,
        serde_json::to_string_pretty(&document).unwrap(),
    )
    .unwrap();
    let after = snapshot(&h, &cookie).await;
    assert_eq!(before, after);
    let override_body = json!({"enabled":true,"threshold":0.9,"version":after["version"],"rules_revision":after["rules_revision"],"topics":["任意问题"]});
    assert_eq!(
        h.request(
            "PUT",
            "/api/communications/takeover",
            Some(&cookie),
            override_body
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    communications::sync::step(&h.state).await.unwrap();
    takeover::step(&h.state).await.unwrap();
    {
        let f = f.lock().unwrap();
        assert_eq!(f.decisions.len(), 1);
        assert_eq!(f.decisions[0]["questions"].as_object().unwrap().len(), 2);
        let topics: Vec<&Value> = f.decisions[0]["questions"]
            .as_object()
            .unwrap()
            .values()
            .map(|v| &v["instructions"]["allowed_topic"])
            .collect();
        assert!(topics.contains(&&document["questions"][0]));
        assert!(topics.contains(&&document["questions"][1]));
        // 夹具给两个问题都打高分，生产逻辑应因多重命中保持静默。
        assert!(f.sent.is_empty());
    }
    server.abort();
    h.close().await;
}

// 验证无效文件使队列失效但采集继续、恢复推进边界，以及文件故障期间仍可关闭；不测试文件系统瞬时故障或线上热部署。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn takeover_invalid_file_pauses_without_interrupting_collection_and_recovers() {
    let (h, f, server) = setup_takeover().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let id = add(&h, &cookie).await;
    enable(&h, &cookie).await;
    communications::sync::step(&h.state).await.unwrap();
    write_questions(&h, json!({"questions":["重复", "重复"]}));
    takeover::step(&h.state).await.unwrap();
    let invalid = snapshot(&h, &cookie).await;
    assert_eq!(invalid["rules_error"], "takeover_rules_invalid");
    assert_eq!(invalid["topics"], json!([]));
    assert!(invalid["enabled"].as_bool().unwrap());
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT reason FROM communication_takeover_jobs")
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        "rules_changed"
    );
    sqlx::query("UPDATE communication_sources SET last_synced_at=NULL WHERE id=$1")
        .bind(id)
        .execute(&h.state.pool)
        .await
        .unwrap();
    due(&h).await;
    communications::sync::step(&h.state).await.unwrap();
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT last_synced_at IS NOT NULL AND error IS NULL FROM communication_sources WHERE id=$1")
            .bind(id)
            .fetch_one(&h.state.pool)
            .await
            .unwrap()
    );
    assert!(f.lock().unwrap().decisions.is_empty());
    write_questions(&h, json!({"questions":["申请 novita 测试环境权限"]}));
    takeover::step(&h.state).await.unwrap();
    let restored = snapshot(&h, &cookie).await;
    assert!(restored["rules_error"].is_null());
    assert!(restored["version"].as_i64().unwrap() > invalid["version"].as_i64().unwrap());
    assert!(restored["since_ms"].as_i64().unwrap() >= invalid["since_ms"].as_i64().unwrap());
    std::fs::remove_file(&h.state.config.takeover_questions_file).unwrap();
    let missing = snapshot(&h, &cookie).await;
    assert_eq!(missing["rules_error"], "takeover_rules_unreadable");
    let disabled = json!({"enabled":false,"threshold":0.9,"version":missing["version"]});
    assert_eq!(
        h.request(
            "PUT",
            "/api/communications/takeover",
            Some(&cookie),
            disabled
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(f.lock().unwrap().sent.is_empty());
    server.abort();
    h.close().await;
}

// 验证私人记忆和沟通原文即使包含相关流程也不能直接用于接管；不证明真实模型语义或生产租户投递。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn takeover_excludes_unpublished_communication_and_personal_memory() {
    let (h, f, server) = setup_takeover().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    add(&h, &cookie).await;
    enable(&h, &cookie).await;
    f.lock().unwrap().communication_knowledge = true;
    memory::create(
        &h,
        &cookie,
        "project.novita.access",
        "申请 novita 测试环境权限：提交申请表，填写测试用途，由环境管理员审核。",
        "project",
    )
    .await;
    communications::sync::step(&h.state).await.unwrap();
    takeover::step(&h.state).await.unwrap();
    let status: String = sqlx::query_scalar("SELECT status FROM communication_takeover_jobs")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(status, "ignored");
    assert!(f.lock().unwrap().sent.is_empty());
    server.abort();
    h.close().await;
}
