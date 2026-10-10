use super::*;
use agent_runtime::tools::Host as _;
use chrono::{Duration as Delta, Utc};
use orbit_server::todos::{self, identity, service};

/// 测试显式插入已验证绑定，不把夹具身份当作真实 OAuth 验收。
async fn bind(h: &Harness) {
    sqlx::query("INSERT INTO personal_identities(owner) VALUES('feishu:ou_allowed')")
        .execute(&h.state.pool)
        .await
        .unwrap();
}
/// 各测试通过生产 API 创建事项，时间推进仅作用于隔离 schema。
fn input(kind: Option<&str>, recurrence: &str) -> Value {
    let mut body = json!({"idempotency_key":Uuid::new_v4(),"content":{"title":"发布准备","objective":"本周发布","next_action":"核对测试结果"}});
    if let Some(kind) = kind {
        body["schedule"] = json!({"kind":kind,"next_run_at":(Utc::now()+Delta::hours(1)).to_rfc3339(),"timezone":"Asia/Shanghai","recurrence":recurrence,"missed_policy":"latest","grace_minutes":1440,"instruction":if kind=="execute" {"整理当前事项并报告缺失的信息"} else {""},"channel":"web"});
    }
    body
}
async fn create(h: &Harness, cookie: &str, body: Value) -> Uuid {
    let (status, _, result) = h.request("POST", "/api/todos", Some(cookie), body).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    result["id"].as_str().unwrap().parse().unwrap()
}
async fn detail(h: &Harness, id: Uuid) -> Value {
    service::detail(&h.state, "admin", id).await.unwrap()
}
async fn due(h: &Harness, id: Uuid) {
    sqlx::query("UPDATE todo_schedules SET next_run_at=now()-interval '1 second',anchor_at=now()-interval '1 second' WHERE todo_id=$1")
        .bind(id).execute(&h.state.pool).await.unwrap();
}

// 验证跨渠道同一事项、创建幂等、旧版本拒绝和非本人拒绝；不验证真实飞书投递。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn shared_identity_and_concurrent_updates() {
    let h = Harness::new().await;
    let cookie = h.login().await;
    bind(&h).await;
    let body = input(None, "once");
    let id = create(&h, &cookie, body.clone()).await;
    assert_eq!(create(&h, &cookie, body).await, id);
    let shared = service::detail(&h.state, "feishu:ou_allowed", id)
        .await
        .unwrap();
    assert_eq!(shared["item"]["title"], "发布准备");
    let update:todos::Update=serde_json::from_value(json!({"version":1,"content":{"title":"发布准备","next_action":"确认窗口"},"status":"waiting_external"})).unwrap();
    service::update(&h.state, "feishu:ou_allowed", id, &update, None)
        .await
        .unwrap();
    assert_eq!(
        service::update(&h.state, "admin", id, &update, None)
            .await
            .unwrap_err()
            .1,
        "todo_version_conflict"
    );
    assert_eq!(
        service::detail(&h.state, "feishu:other", id)
            .await
            .unwrap_err()
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(detail(&h, id).await["item"]["next_action"], "确认窗口");
    let response = h
        .request("GET", "/api/todos?view=active", Some(&cookie), json!({}))
        .await;
    assert_eq!(response.0, StatusCode::OK, "{}", response.2);
    assert_eq!(response.2["total"], 1);
    // 选择器只读取本人可见标题，不展开私有正文。
    let resources = h
        .request(
            "GET",
            "/api/todos/resources?q=材料",
            Some(&cookie),
            json!({}),
        )
        .await;
    assert_eq!(resources.0, StatusCode::OK, "{}", resources.2);
    assert!(resources.2.is_array());
    h.close().await;
}

// 验证并发调度仅产生一期、发送不完成事项、完成停止后续周期；不代表准点 SLA。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn recurring_delivery_and_business_completion() {
    let h = Harness::new().await;
    let cookie = h.login().await;
    let mut body = input(Some("reminder"), "daily");
    body["schedule"]["instruction"] = json!("携带测试报告");
    let id = create(&h, &cookie, body).await;
    due(&h, id).await;
    let (a, b) = tokio::join!(
        todos::scheduler::process_one(&h.state),
        todos::scheduler::process_one(&h.state)
    );
    assert!(a.unwrap() || b.unwrap());
    orbit_server::followups::scheduler::process_one(&h.state, "reminder")
        .await
        .unwrap();
    let d = detail(&h, id).await;
    assert_eq!(d["runs"].as_array().unwrap().len(), 1);
    assert_eq!(d["runs"][0]["status"], "sent");
    assert_eq!(d["item"]["status"], "active");
    assert!(d["runs"][0]["result"].as_str().unwrap().contains("提醒你"));
    assert!(
        d["runs"][0]["result"]
            .as_str()
            .unwrap()
            .contains("携带测试报告")
    );
    let update: todos::Update = serde_json::from_value(
        json!({"version":1,"content":{"title":"发布准备"},"status":"completed"}),
    )
    .unwrap();
    service::update(&h.state, "admin", id, &update, None)
        .await
        .unwrap();
    assert_eq!(detail(&h, id).await["schedules"][0]["status"], "ended");
    assert!(!todos::scheduler::process_one(&h.state).await.unwrap());
    h.close().await;
}

// 验证暂停取消当前期次、恢复需要未来时间、换渠道修改仍保留投递目标；不调用远端平台。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn schedule_pause_resume_and_channel() {
    let h = Harness::new().await;
    let cookie = h.login().await;
    bind(&h).await;
    let mut body = input(Some("reminder"), "weekly");
    body["schedule"]["channel"] = json!("feishu");
    let id = create(&h, &cookie, body.clone()).await;
    due(&h, id).await;
    todos::scheduler::process_one(&h.state).await.unwrap();
    let d = detail(&h, id).await;
    let schedule = &d["schedules"][0];
    service::schedule(&h.state,"admin",id,&json!({"todo_version":1,"id":schedule["id"],"version":1,"status":"paused","idempotency_key":Uuid::new_v4()}),None).await.unwrap();
    let d = detail(&h, id).await;
    assert_eq!(d["runs"][0]["status"], "cancelled");
    assert_eq!(d["schedules"][0]["channel"], "feishu");
    let resume = json!({"todo_version":2,"id":schedule["id"],"version":2,"status":"enabled","idempotency_key":Uuid::new_v4(),"schedule":body["schedule"]});
    service::schedule(&h.state, "feishu:ou_allowed", id, &resume, None)
        .await
        .unwrap();
    assert_eq!(detail(&h, id).await["schedules"][0]["version"], 3);
    assert_eq!(detail(&h, id).await["item"]["version"], 3);
    h.close().await;
}

// 验证长时间停机只补最近一期、skip 保留跳过记录；不验证任意 cron 或节假日日历。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn missed_period_policy() {
    let h = Harness::new().await;
    let cookie = h.login().await;
    let id = create(&h, &cookie, input(Some("reminder"), "daily")).await;
    sqlx::query("UPDATE todo_schedules SET next_run_at=now()-interval '10 days 1 hour',anchor_at=now()-interval '10 days 1 hour' WHERE todo_id=$1").bind(id).execute(&h.state.pool).await.unwrap();
    todos::scheduler::process_one(&h.state).await.unwrap();
    let d = detail(&h, id).await;
    assert_eq!(d["runs"].as_array().unwrap().len(), 1);
    assert_eq!(d["runs"][0]["status"], "scheduled");
    let scheduled =
        chrono::DateTime::parse_from_rfc3339(d["runs"][0]["scheduled_at"].as_str().unwrap())
            .unwrap();
    assert!(scheduled > Utc::now() - Delta::hours(2));
    let mut body = input(Some("reminder"), "daily");
    body["schedule"]["missed_policy"] = json!("skip");
    let skipped = create(&h, &cookie, body).await;
    sqlx::query("UPDATE todo_schedules SET next_run_at=now()-interval '3 days',anchor_at=now()-interval '3 days' WHERE todo_id=$1").bind(skipped).execute(&h.state.pool).await.unwrap();
    todos::scheduler::process_one(&h.state).await.unwrap();
    assert_eq!(detail(&h, skipped).await["runs"][0]["status"], "skipped");
    h.close().await;
}

// 验证解除绑定后读写、待发送期次与旧工具租约失效，网页保留事项；不验证平台已接受请求的撤回。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn unbinding_fences_pending_work() {
    let h = Harness::new().await;
    let cookie = h.login().await;
    bind(&h).await;
    let mut body = input(Some("reminder"), "daily");
    body["schedule"]["channel"] = json!("feishu");
    let id = create(&h, &cookie, body).await;
    due(&h, id).await;
    todos::scheduler::process_one(&h.state).await.unwrap();
    let d = detail(&h, id).await;
    let followup: Uuid = d["runs"][0]["followup_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        todos::scheduler::delivery_allowed(&h.state, followup)
            .await
            .unwrap()
    );
    identity::set_binding(&h.state, "feishu:ou_allowed", false, 1)
        .await
        .unwrap();
    assert!(
        !todos::scheduler::delivery_allowed(&h.state, followup)
            .await
            .unwrap()
    );
    assert!(
        service::detail(&h.state, "feishu:ou_allowed", id)
            .await
            .is_err()
    );
    assert_eq!(detail(&h, id).await["item"]["status"], "active");
    assert_eq!(detail(&h, id).await["runs"][0]["status"], "cancelled");
    h.close().await;
}

// 验证旧提醒自动归入新模型且送达不推断完成，通知在本人两端可见；不模拟真实 OAuth。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn legacy_reminder_shared_projection() {
    let h = Harness::new().await;
    let cookie = h.login().await;
    bind(&h).await;
    let conversation:Uuid=sqlx::query_scalar("INSERT INTO conversations(id,owner,channel,title) VALUES($1,'feishu:ou_allowed','feishu','夹具') RETURNING id").bind(Uuid::new_v4()).fetch_one(&h.state.pool).await.unwrap();
    let result = orbit_server::followups::create(
        &h.state,
        "feishu:ou_allowed",
        &orbit_server::followups::Create {
            communication: None,
            idempotency_key: Uuid::new_v4(),
            conversation_id: Some(conversation),
            kind: "reminder".into(),
            topic: "旧提醒".into(),
            due_at: (Utc::now() + Delta::hours(1)).to_rfc3339(),
            expires_at: None,
            memory_ids: vec![],
        },
        None,
    )
    .await
    .unwrap();
    let id: Uuid = result["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(detail(&h, id).await["item"]["title"], "旧提醒");
    let prefs = orbit_server::followups::preferences(&h.state, "admin")
        .await
        .unwrap();
    assert_eq!(
        orbit_server::followups::preferences(&h.state, "feishu:ou_allowed")
            .await
            .unwrap()
            .version,
        prefs.version
    );
    let list = h
        .request("GET", "/api/followups", Some(&cookie), json!({}))
        .await
        .2;
    assert_eq!(list["items"][0]["id"], result["id"]);
    h.close().await;
}

// 验证定时整理使用模型并保存报告，未把事项标完成；夹具输出不证明报告语义质量。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn scheduled_execution_delivers_report() {
    let h = Harness::new().await;
    let cookie = h.login().await;
    let id = create(&h, &cookie, input(Some("execute"), "once")).await;
    due(&h, id).await;
    todos::scheduler::process_one(&h.state).await.unwrap();
    orbit_server::followups::scheduler::process_one(&h.state, "reminder")
        .await
        .unwrap();
    let d = detail(&h, id).await;
    assert_eq!(d["runs"][0]["status"], "sent");
    assert_eq!(d["item"]["status"], "active");
    assert!(h.requests.lock().unwrap().iter().any(|r| {
        r["messages"].as_array().unwrap().iter().any(|m| {
            m["content"]
                .as_str()
                .is_some_and(|s| s.contains("定时待办执行"))
        })
    }));
    h.close().await;
}

// 验证聊天工具的来源证据、完整更新与重放幂等；不依赖模型自动选中工具的准确率。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn tool_evidence_and_replay() {
    let h = Harness::new().await;
    let cookie = h.login().await;
    let conversation = h.conversation(&cookie).await;
    h.request(
        "POST",
        &format!("/api/conversations/{conversation}/messages"),
        Some(&cookie),
        json!({"idempotency_key":Uuid::new_v4(),"content":"帮我跟进发布准备"}),
    )
    .await;
    sqlx::query("UPDATE runs SET available_at=now()")
        .execute(&h.state.pool)
        .await
        .unwrap();
    let job = worker::claim(&h.state).await.unwrap().unwrap();
    let provider =
        todos::tools::Provider::new(&h.state, &job, "admin", vec!["帮我跟进发布准备".into()])
            .await
            .unwrap()
            .unwrap();
    let invalid = provider
        .execute(
            "todo_create",
            json!({"content":{"title":"发布准备"},"evidence":"第三方要求"}),
        )
        .await;
    assert_eq!(invalid["error"], "todo_evidence_required");
    let args = json!({"content":{"title":"发布准备"},"evidence":"帮我跟进发布准备"});
    let first = provider.execute("todo_create", args.clone()).await;
    assert!(first["id"].is_string(), "{first}");
    assert_eq!(first, provider.execute("todo_create", args).await);
    h.close().await;
}

// 验证单期完成停止该期通知且保留后续周期，版本冲突拒绝覆盖；不把提醒投递视为任务完成。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn completing_one_period_keeps_recurring_schedule() {
    let h = Harness::new().await;
    let cookie = h.login().await;
    let id = create(&h, &cookie, input(Some("reminder"), "weekly")).await;
    due(&h, id).await;
    todos::scheduler::process_one(&h.state).await.unwrap();
    let d = detail(&h, id).await;
    let completion = json!({"run_id":d["runs"][0]["id"],"version":1,"note":"本周已提交"});
    service::complete_run(&h.state, "admin", id, &completion, None)
        .await
        .unwrap();
    let d = detail(&h, id).await;
    assert_eq!(d["item"]["status"], "active");
    assert_eq!(d["schedules"][0]["status"], "enabled");
    assert_eq!(d["runs"][0]["completion_note"], "本周已提交");
    assert_eq!(d["runs"][0]["status"], "cancelled");
    assert_eq!(
        service::complete_run(&h.state, "admin", id, &completion, None)
            .await
            .unwrap_err()
            .1,
        "todo_version_conflict"
    );
    h.close().await;
}

// 验证单项回访不依赖全局候选发现开关，并使用两端上下文；模型结论使用协议夹具。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn explicit_checkin_shares_context() {
    let h = Harness::new().await;
    let cookie = h.login().await;
    bind(&h).await;
    let mut prefs = orbit_server::followups::preferences(&h.state, "admin")
        .await
        .unwrap();
    assert!(!prefs.enabled);
    prefs.quiet_start = 0;
    prefs.quiet_end = 0;
    orbit_server::followups::save_preferences(&h.state, "admin", &prefs)
        .await
        .unwrap();
    let id = create(&h, &cookie, input(Some("checkin"), "once")).await;
    // 发现开关已关闭时保存节奏偏好，不应撤销本人对单项回访的授权。
    let prefs = orbit_server::followups::preferences(&h.state, "admin")
        .await
        .unwrap();
    orbit_server::followups::save_preferences(&h.state, "admin", &prefs)
        .await
        .unwrap();
    assert_eq!(detail(&h, id).await["schedules"][0]["status"], "enabled");
    let conv:Uuid=sqlx::query_scalar("INSERT INTO conversations(id,owner,channel,title) VALUES($1,'feishu:ou_allowed','feishu','本人飞书') RETURNING id").bind(Uuid::new_v4()).fetch_one(&h.state.pool).await.unwrap();
    let run = Uuid::new_v4();
    sqlx::query("INSERT INTO runs(id,conversation_id,idempotency_key,input,status,batch_id,created_at) VALUES($1,$2,$3,'已拿到测试报告','completed',$1,now()-interval '1 hour')").bind(run).bind(conv).bind(run.to_string()).execute(&h.state.pool).await.unwrap();
    sqlx::query("INSERT INTO messages(conversation_id,run_id,role,content) VALUES($1,$2,'user','已拿到测试报告')").bind(conv).bind(run).execute(&h.state.pool).await.unwrap();
    due(&h, id).await;
    todos::scheduler::process_one(&h.state).await.unwrap();
    orbit_server::followups::scheduler::process_one(&h.state, "checkin")
        .await
        .unwrap();
    let d = detail(&h, id).await;
    assert_eq!(d["runs"][0]["status"], "sent");
    assert!(
        h.requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.to_string().contains("已拿到测试报告"))
    );
    h.close().await;
}

// 验证在报告生成后来源版本变化会拦截投递，不覆盖供应商数据本身的真实性。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn execution_source_revision_fences_delivery() {
    let h = Harness::new().await;
    let cookie = h.login().await;
    let id = create(&h, &cookie, input(Some("execute"), "once")).await;
    due(&h, id).await;
    todos::scheduler::process_one(&h.state).await.unwrap();
    let d = detail(&h, id).await;
    let followup: Uuid = d["runs"][0]["followup_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    // 保存一个过时代次模拟投递队列等待期间的资料变化，不伪造真实模型内容。
    sqlx::query("UPDATE followups SET memory_versions=jsonb_build_object('_todo_execution_revision',jsonb_build_object('sources','stale')) WHERE id=$1").bind(followup).execute(&h.state.pool).await.unwrap();
    let job: orbit_server::followups::Followup = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {} FROM followups WHERE id=$1",
        orbit_server::followups::COLUMNS
    )))
    .bind(followup)
    .fetch_one(&h.state.pool)
    .await
    .unwrap();
    assert!(
        !orbit_server::followups::scheduler::can_deliver(&h.state, &job, None)
            .await
            .unwrap()
    );
    h.close().await;
}

// 在独立旧版 schema 中真实应用升级脚本，验证历史消息、状态及来源保留；不操作开发数据库。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn migration_preserves_existing_history() {
    let url = std::env::var("TEST_DATABASE_URL").unwrap();
    let admin = PgPool::connect(&url).await.unwrap();
    let schema = format!("upgrade_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&admin)
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(
            PgConnectOptions::from_str(&url)
                .unwrap()
                .options([("search_path", schema.as_str())]),
        )
        .await
        .unwrap();
    let migrator = sqlx::migrate!("./migrations");
    for migration in migrator.iter().filter(|m| m.version < 24) {
        // SQL 全部来自编译进测试的固定迁移文件，没有插值用户输入。
        sqlx::raw_sql(sqlx::AssertSqlSafe(migration.sql.as_str()))
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO communication_connections(owner,open_id,name,credentials,expires_at,refresh_expires_at) VALUES('admin','old_owner','旧本人',''::bytea,now(),now())").execute(&pool).await.unwrap();
    let conversation = Uuid::new_v4();
    sqlx::query("INSERT INTO conversations(id,owner,channel,title) VALUES($1,'feishu:old_owner','feishu','历史会话')").bind(conversation).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO followup_preferences(owner) VALUES('feishu:old_owner')")
        .execute(&pool)
        .await
        .unwrap();
    let sent = Uuid::new_v4();
    let cancelled = Uuid::new_v4();
    for (id, status) in [(sent, "sent"), (cancelled, "cancelled")] {
        sqlx::query("INSERT INTO followups(id,owner,conversation_id,kind,topic,due_at,expires_at,timezone,status,origin_key,request_hash) VALUES($1,'feishu:old_owner',$2,'reminder','旧材料提醒',now(),now()+interval '1 day','Asia/Shanghai',$3,$1::text,'old')").bind(id).bind(conversation).bind(status).execute(&pool).await.unwrap();
    }
    sqlx::query("INSERT INTO messages(conversation_id,role,content,kind,followup_id,followup_version) VALUES($1,'assistant','历史提醒正文','followup',$2,1)").bind(conversation).bind(sent).execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!("../../migrations/0024_todos.sql"))
        .execute(&pool)
        .await
        .unwrap();
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as("SELECT id,owner,status FROM todos")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(rows.contains(&(sent, "admin".into(), "active".into())));
    assert!(rows.contains(&(cancelled, "admin".into(), "cancelled".into())));
    let result: String = sqlx::query_scalar("SELECT result FROM todo_runs WHERE id=$1")
        .bind(sent)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(result, "历史提醒正文");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM todo_links")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    pool.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
