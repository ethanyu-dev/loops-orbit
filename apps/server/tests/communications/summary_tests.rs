use super::*;
use std::collections::VecDeque;

/// 独立模型脚本只模拟响应协议、失败及在途时序，不调用真实供应商。
struct ModelScript {
    /// 每次请求消费一个响应，意外调用直接使测试失败。
    responses: VecDeque<(StatusCode, Value, Option<Arc<MessageGate>>)>,
    /// 捕获请求以核对纠错范围和调用次数。
    requests: Vec<Value>,
}
/// 为测试服务安装可控模型，真实 PostgreSQL、文件写入和业务路由保持不变。
async fn model(
    h: &mut Harness,
    responses: Vec<(StatusCode, Value, Option<Arc<MessageGate>>)>,
) -> (Arc<Mutex<ModelScript>>, tokio::task::JoinHandle<()>) {
    let script = Arc::new(Mutex::new(ModelScript {
        responses: responses.into(),
        requests: vec![],
    }));
    let app = Router::new().route("/chat/completions", post(|State(script): State<Arc<Mutex<ModelScript>>>, Json(body): Json<Value>| async move {
        let (status, value, gate) = {
            let mut script = script.lock().unwrap();
            script.requests.push(body);
            script.responses.pop_front().expect("不得发出脚本之外的模型请求")
        };
        if let Some(gate) = gate { gate.arrived.notify_one(); gate.release.notified().await; }
        (status, Json(json!({"choices":[{"message":{"content":value.to_string()}}]})))
    })).with_state(script.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = (*h.state.config).clone();
    config.model.base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    h.state = AppState::new(config, h.state.pool.clone()).unwrap();
    h.app = router(h.state.clone());
    (script, server)
}
/// 默认脚本响应，不附带延迟或真实网络故障。
fn response(value: Value) -> (StatusCode, Value, Option<Arc<MessageGate>>) {
    (StatusCode::OK, value, None)
}
/// 固定候选只用夹具消息的连续原文，身份字段由实际服务补齐。
fn item(id: &str, quote: &str) -> Value {
    json!({"kind":"fact_candidate","text":"核对材料安排","message_id":id,"quote":quote})
}
/// 导入原文但不启动后台 worker，测试显式推进领取和重试。
async fn raw(h: &Harness, cookie: &str) -> Uuid {
    connect(h, cookie).await;
    add(h, cookie).await;
    for _ in 0..2 {
        due(h).await;
        communications::sync::step(&h.state).await.unwrap();
    }
    sqlx::query_scalar("SELECT id FROM communication_documents LIMIT 1")
        .fetch_one(&h.state.pool)
        .await
        .unwrap()
}
/// 从正式路由读取详情，包含真实摘要文件和证据复核。
async fn detail(h: &Harness, cookie: &str, id: Uuid) -> Value {
    let (status, _, value) = h
        .request(
            "GET",
            &format!("/api/communications/documents/{id}"),
            Some(cookie),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}
/// 仅提前测试调度时间，不重置已领取次数或终止状态。
async fn advance(h: &Harness) {
    sqlx::query("UPDATE communication_documents SET next_summary=now()-interval '1 second'")
        .execute(&h.state.pool)
        .await
        .unwrap();
}

// 验证只纠错失败候选、纠错仍受原文校验、正确条目不重做；不评估真实模型的纠错成功率。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn summary_repairs_only_rejected_candidates() {
    let (mut h, _, feishu) = setup().await;
    let good = item("om_me", "我明天把材料发给你");
    let (script, server) = model(
        &mut h,
        vec![
            response(json!([good, item("missing", "我来整理散步调研材料")])),
            response(json!([{"index":1,"item":item("om_other", "我来整理散步调研材料")}])),
        ],
    )
    .await;
    let cookie = h.login().await;
    let id = raw(&h, &cookie).await;
    assert!(communications::summary_jobs::step(&h.state).await.unwrap());
    let value = detail(&h, &cookie, id).await;
    assert_eq!(value["document"]["summary_status"], "ready");
    assert_eq!(value["summary"]["items"].as_array().unwrap().len(), 2);
    assert_eq!(value["summary"]["rejected_count"], 0);
    assert_eq!(value["summary"]["items"][1]["sender_id"], "ou_other");
    {
        let script = script.lock().unwrap();
        assert_eq!(script.requests.len(), 2);
        let repair = &script.requests[1];
        assert!(repair.get("tools").is_none());
        let input: Value = serde_json::from_str(
            repair["messages"].as_array().unwrap().last().unwrap()["content"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(input["rejected"].as_array().unwrap().len(), 1);
        assert_eq!(input["rejected"][0]["index"], 1);
        assert_eq!(
            input["rejected"][0]["error"],
            "communication_summary_unknown_message"
        );
    }
    assert!(!communications::summary_jobs::step(&h.state).await.unwrap());
    server.abort();
    feishu.abort();
    h.close().await;
}

// 验证部分结果、统计、提醒绑定、重排授权和版本去重；不投递实际提醒或验收浏览器交互。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn summary_partial_retry_cancels_old_references_and_deduplicates() {
    let (mut h, _, feishu) = setup().await;
    let good = item("om_me", "我明天把材料发给你");
    let (script, server) = model(
        &mut h,
        vec![
            response(json!([good, item("om_other", "不存在的原话")])),
            response(json!([{"index":1,"item":null}])),
            response(json!([good])),
        ],
    )
    .await;
    let cookie = h.login().await;
    let id = raw(&h, &cookie).await;
    communications::summary_jobs::step(&h.state).await.unwrap();
    let value = detail(&h, &cookie, id).await;
    assert_eq!(value["document"]["summary_status"], "partial");
    assert_eq!(value["summary"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(value["summary"]["rejected_count"], 1);
    let version = value["document"]["version"].as_i64().unwrap();
    communications::progress::step(&h.state).await.unwrap();
    let stats = h
        .request(
            "GET",
            "/api/communications/status",
            Some(&cookie),
            Value::Null,
        )
        .await
        .2;
    assert_eq!(stats["progress"][0]["partial"], 1);
    assert_eq!(stats["progress"][0]["ready"], 0);
    let files = h
        .request(
            "GET",
            "/api/communications/library/files?status=failed",
            Some(&cookie),
            Value::Null,
        )
        .await
        .2;
    assert_eq!(files["items"][0]["summary_status"], "partial");
    let task_body = json!({"idempotency_key":Uuid::new_v4(),"kind":"reminder","topic":"材料","due_at":(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339(),"communication":{"document_id":id,"version":version,"item":0}});
    let (status, _, task) = h
        .request("POST", "/api/followups", Some(&cookie), task_body)
        .await;
    assert_eq!(status, StatusCode::OK, "{task}");
    let url = format!("/api/communications/documents/{id}/summary/retry");
    assert_eq!(
        h.request("POST", &url, None, json!({"version":version}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    sqlx::query("UPDATE communication_sources SET enabled=false")
        .execute(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(
        h.request("POST", &url, Some(&cookie), json!({"version":version}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    sqlx::query("UPDATE communication_sources SET enabled=true")
        .execute(&h.state.pool)
        .await
        .unwrap();
    let (status, _, queued) = h
        .request("POST", &url, Some(&cookie), json!({"version":version}))
        .await;
    assert_eq!(status, StatusCode::OK, "{queued}");
    assert_eq!(queued["version"], version + 1);
    assert_eq!(
        h.request("POST", &url, Some(&cookie), json!({"version":version}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        h.request("POST", &url, Some(&cookie), json!({"version":version+1}))
            .await
            .2,
        queued
    );
    let pending = detail(&h, &cookie, id).await;
    assert_eq!(pending["document"]["summary_attempts"], 0);
    assert!(pending["summary"].is_null());
    let task_status: String = sqlx::query_scalar("SELECT status FROM followups WHERE id=$1")
        .bind(Uuid::parse_str(task["id"].as_str().unwrap()).unwrap())
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(task_status, "cancelled");
    assert_eq!(
        script.lock().unwrap().requests.len(),
        2,
        "刷新及重排不能在 HTTP 请求内执行模型"
    );
    communications::summary_jobs::step(&h.state).await.unwrap();
    assert_eq!(
        detail(&h, &cookie, id).await["document"]["summary_status"],
        "ready"
    );
    server.abort();
    feishu.abort();
    h.close().await;
}

// 验证全失败、重复或越界纠错序号不会变成空成功；不判断模型拒绝修复的语义原因。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn summary_all_rejected_stops_without_empty_success() {
    for repair in [
        json!([]),
        json!([{"index":0,"item":null}]),
        json!([{"index":7,"item":item("om_me","我明天把材料发给你")}]),
        json!([{"index":0,"item":item("om_me","我明天把材料发给你")},{"index":0,"item":item("om_me","我明天把材料发给你")}]),
    ] {
        let (mut h, _, feishu) = setup().await;
        let (script, server) = model(
            &mut h,
            vec![
                response(json!([item("om_me", "伪造引用")])),
                response(repair),
            ],
        )
        .await;
        let cookie = h.login().await;
        let id = raw(&h, &cookie).await;
        communications::summary_jobs::step(&h.state).await.unwrap();
        let value = detail(&h, &cookie, id).await;
        assert_eq!(value["document"]["summary_status"], "failed");
        assert_eq!(
            value["document"]["summary_error"],
            "communication_summary_quote_mismatch"
        );
        assert!(value["summary"].is_null());
        advance(&h).await;
        assert!(!communications::summary_jobs::step(&h.state).await.unwrap());
        assert_eq!(script.lock().unwrap().requests.len(), 2);
        server.abort();
        feishu.abort();
        h.close().await;
    }
}

// 验证临时错误最多三次、永久拒绝不重试及重启租约耗尽恢复；不模拟真实供应商限流策略。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn summary_provider_retries_are_bounded_and_version_resets_budget() {
    let (mut h, _, feishu) = setup().await;
    let (script, server) = model(
        &mut h,
        vec![
            (StatusCode::SERVICE_UNAVAILABLE, Value::Null, None),
            (StatusCode::TOO_MANY_REQUESTS, Value::Null, None),
            (StatusCode::SERVICE_UNAVAILABLE, Value::Null, None),
            (StatusCode::BAD_REQUEST, Value::Null, None),
        ],
    )
    .await;
    let cookie = h.login().await;
    let id = raw(&h, &cookie).await;
    for attempt in 1..=3 {
        advance(&h).await;
        assert!(communications::summary_jobs::step(&h.state).await.unwrap());
        let value = detail(&h, &cookie, id).await;
        assert_eq!(value["document"]["summary_attempts"], attempt);
        assert_eq!(
            value["document"]["summary_status"],
            if attempt < 3 { "retry_wait" } else { "failed" }
        );
        assert!(!communications::summary_jobs::step(&h.state).await.unwrap());
    }
    advance(&h).await;
    assert!(!communications::summary_jobs::step(&h.state).await.unwrap());
    sqlx::query("UPDATE communication_documents SET version=version+1")
        .execute(&h.state.pool)
        .await
        .unwrap();
    let reset = detail(&h, &cookie, id).await;
    assert_eq!(reset["document"]["summary_attempts"], 0);
    assert_eq!(reset["document"]["summary_status"], "pending");
    communications::summary_jobs::step(&h.state).await.unwrap();
    assert_eq!(
        detail(&h, &cookie, id).await["document"]["summary_status"],
        "failed"
    );
    sqlx::query("UPDATE communication_documents SET summary_status='running',summary_attempts=3,next_summary=now()-interval '1 second',summary_error=NULL").execute(&h.state.pool).await.unwrap();
    assert!(!communications::summary_jobs::step(&h.state).await.unwrap());
    assert_eq!(
        detail(&h, &cookie, id).await["document"]["summary_error"],
        "communication_summary_interrupted"
    );
    assert_eq!(script.lock().unwrap().requests.len(), 4);
    server.abort();
    feishu.abort();
    h.close().await;
}

// 在纠错等待期间通过真实采集流程编辑或撤回原文，旧响应必须丢弃；不覆盖跨进程基础设施故障。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn summary_repair_cannot_publish_after_edit_or_recall() {
    for recalled in [false, true] {
        let (mut h, fixture, feishu) = setup().await;
        let gate = Arc::new(MessageGate {
            arrived: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let (_, server) = model(
            &mut h,
            vec![
                response(json!([item("om_me", "错误原话")])),
                (
                    StatusCode::OK,
                    json!([{"index":0,"item":item("om_me","我明天把材料发给你")}]),
                    Some(gate.clone()),
                ),
            ],
        )
        .await;
        let cookie = h.login().await;
        let id = raw(&h, &cookie).await;
        let state = h.state.clone();
        let worker = tokio::spawn(async move { communications::summary_jobs::step(&state).await });
        tokio::time::timeout(Duration::from_secs(5), gate.arrived.notified())
            .await
            .unwrap();
        {
            let mut f = fixture.lock().unwrap();
            f.edited = !recalled;
            f.recalled = recalled;
        }
        sqlx::query(
            "UPDATE communication_sources SET audit_at=now()-interval '1 second',next_sync=now()",
        )
        .execute(&h.state.pool)
        .await
        .unwrap();
        communications::sync::step(&h.state).await.unwrap();
        gate.release.notify_one();
        worker.await.unwrap().unwrap();
        let current = detail(&h, &cookie, id).await;
        assert!(current["summary"].is_null());
        assert_eq!(current["document"]["summary_status"], "pending");
        assert_eq!(current["document"]["summary_attempts"], 0);
        server.abort();
        feishu.abort();
        h.close().await;
    }
}

// 验证多块任务后续模型失败时保留前块结果并报告覆盖缺口；大文本是本地夹具，不代表真实消息容量验收。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn summary_later_chunk_failure_keeps_verified_items() {
    let (mut h, _, feishu) = setup().await;
    let (_, server) = model(
        &mut h,
        vec![
            response(json!([item("om_me", "我明天把材料发给你")])),
            (StatusCode::SERVICE_UNAVAILABLE, Value::Null, None),
        ],
    )
    .await;
    let cookie = h.login().await;
    let id = raw(&h, &cookie).await;
    let value = detail(&h, &cookie, id).await;
    let mut messages = value["messages"].as_array().unwrap().clone();
    let mut third = messages[1].clone();
    third["message_id"] = json!("om_third");
    messages.push(third);
    for message in &mut messages {
        message["text"] = json!(format!(
            "{}{}",
            message["text"].as_str().unwrap(),
            "x".repeat(12500)
        ));
    }
    let text = messages
        .iter()
        .map(|m| format!("{m}\n"))
        .collect::<String>();
    let hash = auth::hash(&text);
    let path = h
        .state
        .config
        .memory
        .as_ref()
        .unwrap()
        .directory
        .join("_communications")
        .join(value["document"]["source_id"].as_str().unwrap())
        .join(id.to_string())
        .join(format!("{hash}.jsonl"));
    std::fs::write(path, text).unwrap();
    sqlx::query("UPDATE communication_documents SET raw_hash=$2,version=version+1 WHERE id=$1")
        .bind(id)
        .bind(hash)
        .execute(&h.state.pool)
        .await
        .unwrap();
    communications::summary_jobs::step(&h.state).await.unwrap();
    let current = detail(&h, &cookie, id).await;
    assert_eq!(current["document"]["summary_status"], "partial");
    assert_eq!(current["summary"]["failed_chunk_count"], 2);
    assert_eq!(current["summary"]["rejected_count"], 0);
    assert_eq!(current["summary"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        current["summary"]["items"][0]["quote"],
        "我明天把材料发给你"
    );
    advance(&h).await;
    assert!(!communications::summary_jobs::step(&h.state).await.unwrap());
    server.abort();
    feishu.abort();
    h.close().await;
}
