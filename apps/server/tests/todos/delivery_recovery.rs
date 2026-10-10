use super::*;

// 验证实际宿主继承飞书、拒绝猜测切换、接受正向依据，及网页修改保留渠道；不调用真实飞书。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn channel_inheritance_and_explicit_override() {
    let h = Harness::new().await;
    let cookie = h.login().await;
    bind(&h).await;
    let conversation = h.conversation(&cookie).await;
    sqlx::query("UPDATE conversations SET owner='feishu:ou_allowed',channel='feishu' WHERE id=$1")
        .bind(conversation)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let input_text = "每天提醒我。推送到网页";
    h.send(&cookie, conversation, input_text, Uuid::new_v4())
        .await;
    sqlx::query("UPDATE runs SET available_at=now()")
        .execute(&h.state.pool)
        .await
        .unwrap();
    let job = worker::claim(&h.state).await.unwrap().unwrap();
    let provider =
        todos::tools::Provider::new(&h.state, &job, "feishu:ou_allowed", vec![input_text.into()])
            .await
            .unwrap()
            .unwrap();
    let mut args = input(Some("reminder"), "daily");
    args.as_object_mut().unwrap().remove("idempotency_key");
    args["evidence"] = json!("每天提醒我");
    let rejected = provider.execute("todo_create", args.clone()).await;
    assert_eq!(rejected["error"], "todo_channel_evidence_required");
    args["schedule"].as_object_mut().unwrap().remove("channel");
    let saved = provider.execute("todo_create", args.clone()).await;
    assert_eq!(saved["schedule"]["channel"], "feishu", "{saved}");
    assert_eq!(saved, provider.execute("todo_create", args.clone()).await);
    let id: Uuid = saved["id"].as_str().unwrap().parse().unwrap();
    let schedule = &detail(&h, id).await["schedules"][0];
    // 第二个本人绑定用于证明 inherit 保留完整目标，不会因多个身份而重新猜选。
    sqlx::query("INSERT INTO personal_identities(owner) VALUES('feishu:second')")
        .execute(&h.state.pool)
        .await
        .unwrap();
    let changed = service::schedule(
        &h.state,
        "admin",
        id,
        &json!({
            "todo_version":1,"id":schedule["id"],"version":1,"status":"enabled",
            "idempotency_key":Uuid::new_v4(),"schedule":args["schedule"]
        }),
        None,
    )
    .await
    .unwrap();
    assert_eq!(changed["schedule"]["channel"], "feishu");
    assert_eq!(
        detail(&h, id).await["schedules"][0]["conversation_id"],
        schedule["conversation_id"]
    );
    args["schedule"]["channel"] = json!("web");
    args["channel_evidence"] = json!("推送到网页");
    let explicit = provider.execute("todo_create", args).await;
    assert_eq!(explicit["schedule"]["channel"], "web", "{explicit}");
    h.close().await;
}

/// 本地响应只模拟工具调用协议，不连接模型服务。
fn tool_reply(name: &str, args: Value) -> Value {
    json!({"choices":[{"finish_reason":"tool_calls","message":{"tool_calls":[{
        "id":name,"type":"function","function":{"name":name,"arguments":args.to_string()}
    }]}}]})
}

// 真实数据库验证修改提交后连续 503 的回执、错误码和一次写入；仅检查出站队列，不代表飞书送达。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn saved_change_survives_reply_failure_and_restart() {
    let mut h = Harness::new().await;
    let cookie = h.login().await;
    bind(&h).await;
    let body = input(Some("execute"), "daily");
    let id = create(&h, &cookie, body.clone()).await;
    let d = detail(&h, id).await;
    let conversation = h.conversation(&cookie).await;
    let run = h
        .send(&cookie, conversation, "改到飞书", Uuid::new_v4())
        .await;
    sqlx::query("UPDATE runs SET reply_to='fixture-reply',available_at=now() WHERE id=$1")
        .bind(run)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let mut schedule = body["schedule"].clone();
    schedule["channel"] = json!("feishu");
    let args = json!({"id":id,"todo_version":1,"schedule_id":d["schedules"][0]["id"],
        "version":1,"status":"enabled","schedule":schedule,"evidence":"改到飞书","channel_evidence":"改到飞书"});
    let responses = Arc::new(Mutex::new(std::collections::VecDeque::from([
        tool_reply("tools_load", json!({"names":["todo_schedule"]})),
        tool_reply("todo_schedule", args),
    ])));
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = count.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let responses = responses.clone();
            let count = observed.clone();
            async move {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                match responses.lock().unwrap().pop_front() {
                    Some(body) => Json(body).into_response(),
                    None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut model = h.state.config.model.clone();
    model.base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    h.state.runtime = agent_runtime::Runtime::new(model).unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(worker::run_worker(h.state.clone(), receiver));
    wait_for(&h.state, "failed", 1).await;
    stop.send(true).unwrap();
    task.await.unwrap();
    let record: (String, String, i32) =
        sqlx::query_as("SELECT error,phase,attempts FROM runs WHERE id=$1")
            .bind(run)
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert_eq!(
        record,
        ("provider_rejected".into(), "saved_reply_failed".into(), 1)
    );
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 5);
    let d = detail(&h, id).await;
    assert_eq!(d["schedules"][0]["channel"], "feishu");
    assert_eq!(d["schedules"][0]["version"], 2);
    let reply: String = sqlx::query_scalar("SELECT content FROM outbox WHERE id=$1")
        .bind(run)
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert!(reply.contains("飞书投递"));
    assert!(reply.contains("下次执行"));
    assert!(reply.contains("其他步骤是否完成尚未确认"));
    let message: String =
        sqlx::query_scalar("SELECT content FROM messages WHERE run_id=$1 AND role='assistant'")
            .bind(run)
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert_eq!(reply, message);
    // 模拟提交后进程中断、租约恢复，不重放工具或新增出站回复；不验证真实容器崩溃时序。
    sqlx::query("UPDATE runs SET status='queued',available_at=now() WHERE id=$1")
        .bind(run)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(worker::run_worker(h.state.clone(), receiver));
    wait_for(&h.state, "failed", 1).await;
    stop.send(true).unwrap();
    task.await.unwrap();
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 5);
    assert_eq!(detail(&h, id).await["schedules"][0]["version"], 2);
    server.abort();
    h.close().await;
}
