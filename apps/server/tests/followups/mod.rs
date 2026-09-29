use super::*;
use agent_runtime::tools::Host as _;
use chrono::{Duration as Delta, Utc};
use orbit_server::followups::{self as tasks, Create, Followup, Update, scheduler};

/// 通过服务层创建可追踪事项；所有到期推进使用数据库时钟而非长时间等待。
async fn create(
    h: &Harness,
    owner: &str,
    conversation: Option<Uuid>,
    kind: &str,
    topic: &str,
) -> Uuid {
    let result = tasks::create(
        &h.state,
        owner,
        &Create {
            communication: None,
            idempotency_key: Uuid::new_v4(),
            conversation_id: conversation,
            kind: kind.into(),
            topic: topic.into(),
            due_at: (Utc::now() + Delta::hours(1)).to_rfc3339(),
            expires_at: None,
            memory_ids: vec![],
        },
        None,
    )
    .await
    .unwrap();
    Uuid::parse_str(result["id"].as_str().unwrap()).unwrap()
}
/// 读取生产投影，测试不猜测 worker 是否已经提交。
async fn get(h: &Harness, id: Uuid) -> Followup {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {} FROM followups WHERE id=$1",
        tasks::COLUMNS
    )))
    .bind(id)
    .fetch_one(&h.state.pool)
    .await
    .unwrap()
}
/// 用数据库推进到期时间，保留真实领取、事务与版本路径。
async fn due(h: &Harness, id: Uuid) {
    sqlx::query("UPDATE followups SET due_at=now()-interval '1 second' WHERE id=$1")
        .bind(id)
        .execute(&h.state.pool)
        .await
        .unwrap();
}
/// 开启回访并去掉静默，以便测试不依赖运行机器的当地时间。
async fn enable(h: &Harness, owner: &str) {
    let mut prefs = tasks::preferences(&h.state, owner).await.unwrap();
    prefs.enabled = true;
    prefs.quiet_start = 0;
    prefs.quiet_end = 0;
    tasks::save_preferences(&h.state, owner, &prefs)
        .await
        .unwrap();
}
/// 等到模型已读到快照，再提交新的用户操作，构造真实异步竞态。
async fn wait_model(h: &Harness) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if h.requests.lock().unwrap().iter().any(|v| {
                v["messages"][0]["content"]
                    .as_str()
                    .is_some_and(|s| s.contains("判断是否适合主动回访"))
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

// 验证网页创建幂等、独立主动消息、通知已读、并发领取及旧租约恢复；不覆盖浏览器推送。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn reminder_delivery_and_recovery() {
    let h = Harness::new().await;
    let cookie = h.login().await;
    let input = json!({"idempotency_key":Uuid::new_v4(),"kind":"reminder","topic":"交材料","due_at":(Utc::now()+Delta::hours(1)).to_rfc3339()});
    let (status, _, result) = h
        .request("POST", "/api/followups", Some(&cookie), input.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let id = Uuid::parse_str(result["id"].as_str().unwrap()).unwrap();
    let repeat = h
        .request("POST", "/api/followups", Some(&cookie), input.clone())
        .await
        .2;
    assert_eq!(repeat["id"], result["id"]);
    let mut different = input.clone();
    different["topic"] = json!("换一个内容");
    assert_eq!(
        h.request("POST", "/api/followups", Some(&cookie), different)
            .await
            .0,
        StatusCode::CONFLICT
    );
    due(&h, id).await;
    sqlx::query("UPDATE followups SET status='checking',lease_until=now()-interval '1 second',lease_token=$2 WHERE id=$1").bind(id).bind(Uuid::new_v4()).execute(&h.state.pool).await.unwrap();
    let rebuilt = AppState::new((*h.state.config).clone(), h.state.pool.clone()).unwrap();
    let (a, b) = tokio::join!(
        scheduler::process_one(&rebuilt, "reminder"),
        scheduler::process_one(&rebuilt, "reminder")
    );
    assert!(a.unwrap() || b.unwrap());
    assert_eq!(get(&h, id).await.status, "sent");
    let messages: i64 = sqlx::query_scalar("SELECT count(*) FROM messages WHERE followup_id=$1 AND run_id IS NULL AND role='assistant'").bind(id).fetch_one(&h.state.pool).await.unwrap();
    assert_eq!(messages, 1);
    assert!(
        h.requests.lock().unwrap().is_empty(),
        "明确提醒不依赖模型供应商"
    );
    let (_, _, notices) = h
        .request("GET", "/api/notifications", Some(&cookie), json!({}))
        .await;
    assert_eq!(notices["unread"], 1);
    let notice = &notices["items"][0];
    let (_, _, detail) = h
        .request(
            "GET",
            &format!(
                "/api/conversations/{}",
                result["conversation_id"].as_str().unwrap()
            ),
            Some(&cookie),
            json!({}),
        )
        .await;
    assert_eq!(detail["messages"][0]["kind"], "followup");
    assert_eq!(
        h.request(
            "POST",
            &format!("/api/notifications/{}/read", notice["id"]),
            Some(&cookie),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        h.request("GET", "/api/notifications", Some(&cookie), json!({}))
            .await
            .2["unread"],
        0
    );
    h.close().await;
}

// 验证改期后旧版本不能投递、取消与过期不补发；不模拟操作系统进程崩溃。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn reschedule_cancel_expire_and_stale_version() {
    let h = Harness::new().await;
    let id = create(&h, "admin", None, "reminder", "改期事项").await;
    due(&h, id).await;
    sqlx::query("UPDATE followups SET status='checking',lease_token=$2 WHERE id=$1")
        .bind(id)
        .bind(Uuid::new_v4())
        .execute(&h.state.pool)
        .await
        .unwrap();
    let stale = get(&h, id).await;
    let update = Update {
        version: 1,
        status: "scheduled".into(),
        due_at: Some((Utc::now() + Delta::hours(2)).to_rfc3339()),
        topic: None,
    };
    tasks::update(&h.state, "admin", id, &update, None)
        .await
        .unwrap();
    scheduler::queue(&h.state, &stale, "旧正文", None)
        .await
        .unwrap();
    assert_eq!(get(&h, id).await.status, "scheduled");
    assert!(
        tasks::update(&h.state, "admin", id, &update, None)
            .await
            .is_err()
    );
    tasks::update(
        &h.state,
        "admin",
        id,
        &Update {
            version: 2,
            status: "cancelled".into(),
            due_at: None,
            topic: None,
        },
        None,
    )
    .await
    .unwrap();
    due(&h, id).await;
    scheduler::process_one(&h.state, "reminder").await.unwrap();
    let expired = create(&h, "admin", None, "reminder", "过期事项").await;
    sqlx::query("UPDATE followups SET due_at=now()-interval '2 days',expires_at=now()-interval '1 day' WHERE id=$1").bind(expired).execute(&h.state.pool).await.unwrap();
    scheduler::process_one(&h.state, "reminder").await.unwrap();
    assert_eq!(get(&h, expired).await.status, "expired");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM messages WHERE kind='followup'")
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        0
    );
    h.close().await;
}

// 验证回访发送、冷却和关闭开关，但固定模型输出不代表自然表达或语义判断质量。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn checkin_policy_and_opt_out() {
    let h = Harness::new().await;
    assert!(!tasks::preferences(&h.state, "admin").await.unwrap().enabled);
    enable(&h, "admin").await;
    let first = create(&h, "admin", None, "checkin", "新方案").await;
    due(&h, first).await;
    scheduler::process_one(&h.state, "checkin").await.unwrap();
    assert_eq!(get(&h, first).await.status, "sent");
    let second = create(&h, "admin", None, "checkin", "另一件事").await;
    due(&h, second).await;
    scheduler::process_one(&h.state, "checkin").await.unwrap();
    assert_eq!(get(&h, second).await.status, "scheduled");
    let explicit = create(&h, "admin", None, "reminder", "明确提醒").await;
    let mut prefs = tasks::preferences(&h.state, "admin").await.unwrap();
    prefs.enabled = false;
    tasks::save_preferences(&h.state, "admin", &prefs)
        .await
        .unwrap();
    assert_eq!(get(&h, second).await.status, "cancelled");
    due(&h, explicit).await;
    scheduler::process_one(&h.state, "reminder").await.unwrap();
    assert_eq!(get(&h, explicit).await.status, "sent");
    h.close().await;
}

// 验证模型读取后收到新用户输入会放弃旧消息与旧完成判断；不测试真实模型推理。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn user_input_fences_inflight_checkin() {
    let h = Harness::new().await;
    let cookie = h.login().await;
    let conv = h.conversation(&cookie).await;
    enable(&h, "admin").await;
    let id = create(&h, "admin", Some(conv), "checkin", "新方案").await;
    due(&h, id).await;
    let state = h.state.clone();
    let work = tokio::spawn(async move { scheduler::process_one(&state, "checkin").await });
    wait_model(&h).await;
    h.send(&cookie, conv, "先别问，我正在调整", Uuid::new_v4())
        .await;
    work.await.unwrap().unwrap();
    assert_eq!(get(&h, id).await.status, "scheduled");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM messages WHERE kind='followup'")
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        0
    );
    h.close().await;
}

// 验证飞书主动发送走 open_id、失败重试复用 UUID、发送前改期取消旧队列；不连接飞书公网。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn feishu_proactive_retry_and_cancellation() {
    let h = Harness::new().await;
    let conv = Uuid::new_v4();
    sqlx::query("INSERT INTO conversations(id,owner,channel,title) VALUES($1,'feishu:ou_allowed','feishu','夹具')").bind(conv).execute(&h.state.pool).await.unwrap();
    let id = create(&h, "feishu:ou_allowed", Some(conv), "reminder", "飞书事项").await;
    due(&h, id).await;
    scheduler::process_one(&h.state, "reminder").await.unwrap();
    assert_eq!(get(&h, id).await.status, "queued");
    orbit_server::feishu::deliver_one(&h.state).await.unwrap();
    assert_eq!(get(&h, id).await.status, "queued");
    sqlx::query("UPDATE outbox SET available_at=now() WHERE followup_id=$1")
        .bind(id)
        .execute(&h.state.pool)
        .await
        .unwrap();
    orbit_server::feishu::deliver_one(&h.state).await.unwrap();
    assert_eq!(get(&h, id).await.status, "sent");
    {
        let requests = h.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["uuid"], requests[1]["uuid"]);
        assert_eq!(requests[0]["receive_id"], "ou_allowed");
    }
    let cancelled = create(
        &h,
        "feishu:ou_allowed",
        Some(conv),
        "reminder",
        "取消飞书事项",
    )
    .await;
    due(&h, cancelled).await;
    scheduler::process_one(&h.state, "reminder").await.unwrap();
    tasks::update(
        &h.state,
        "feishu:ou_allowed",
        cancelled,
        &Update {
            version: 1,
            status: "cancelled".into(),
            due_at: None,
            topic: None,
        },
        None,
    )
    .await
    .unwrap();
    orbit_server::feishu::deliver_one(&h.state).await.unwrap();
    assert_eq!(h.requests.lock().unwrap().len(), 2);
    h.close().await;
}

// 验证来源工具幂等、伪造证据拒绝与过期租约拒绝；直接宿主调用不验证模型工具选择质量。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn tool_source_evidence_and_lease() {
    let h = Harness::new().await;
    let cookie = h.login().await;
    let conv = h.conversation(&cookie).await;
    let id = h
        .send(
            &cookie,
            conv,
            "明天提醒我交材料，不要主动跟进",
            Uuid::new_v4(),
        )
        .await;
    sqlx::query("UPDATE runs SET status='running',lease_token=$2 WHERE id=$1")
        .bind(id)
        .bind(Uuid::new_v4())
        .execute(&h.state.pool)
        .await
        .unwrap();
    let job:worker::Job=sqlx::query_as("SELECT id,conversation_id,seq,batch_id,input,attempts,lease_token,reply_to FROM runs WHERE id=$1").bind(id).fetch_one(&h.state.pool).await.unwrap();
    let host = tasks::tools::Host::new(&h.state, &job).await.unwrap();
    let args = json!({"topic":"交材料","due_at":(Utc::now()+Delta::days(1)).to_rfc3339(),"evidence":"明天提醒我交材料"});
    let first = host.execute("followup_create", args.clone()).await;
    assert!(first["id"].is_string(), "{first}");
    assert_eq!(
        host.execute("followup_create", args).await["id"],
        first["id"]
    );
    assert!(
        host.execute(
            "followup_preferences",
            json!({"enabled":true,"evidence":"不要主动跟进"})
        )
        .await["error"]
            .is_string()
    );
    let invalid=host.execute("followup_create",json!({"topic":"伪造","due_at":(Utc::now()+Delta::days(1)).to_rfc3339(),"evidence":"用户没说过"})).await;
    assert_eq!(invalid["error"], "followup_evidence_required");
    sqlx::query("UPDATE runs SET status='cancelled',lease_token=NULL WHERE id=$1")
        .bind(id)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let cancelled = host
        .execute(
            "followup_update",
            json!({"id":first["id"],"version":1,"status":"cancelled","evidence":"提醒我交材料"}),
        )
        .await;
    assert_eq!(cancelled["error"], "run_superseded");
    h.close().await;
}

// 验证文件遗忘与在途模型判断联动；真实文件/数据库，模型正文仍为本地夹具。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn memory_forgetting_cancels_inflight_checkin() {
    let h = super::memory::setup(false).await;
    let cookie = h.login().await;
    let memory = super::memory::create(&h, &cookie, "方案", "正在尝试新方案", "project").await;
    enable(&h, "admin").await;
    let input = Create {
        communication: None,
        idempotency_key: Uuid::new_v4(),
        conversation_id: None,
        kind: "checkin".into(),
        topic: "新方案".into(),
        due_at: (Utc::now() + Delta::hours(1)).to_rfc3339(),
        expires_at: None,
        memory_ids: vec![memory.id],
    };
    let result = tasks::create(&h.state, "admin", &input, None)
        .await
        .unwrap();
    let id = Uuid::parse_str(result["id"].as_str().unwrap()).unwrap();
    due(&h, id).await;
    let state = h.state.clone();
    let work = tokio::spawn(async move { scheduler::process_one(&state, "checkin").await });
    wait_model(&h).await;
    orbit_server::memory::forget(&h.state, "admin", memory.id)
        .await
        .unwrap();
    work.await.unwrap().unwrap();
    assert_eq!(get(&h, id).await.status, "cancelled");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM messages WHERE kind='followup'")
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        0
    );
    h.close().await;
}

// 验证开启后的完成轮次发现一次候选和后台重试去重；不评价真实模型的事项提取准确率。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn discovery_from_completed_run() {
    let h = Harness::new().await;
    let cookie = h.login().await;
    let conv = h.conversation(&cookie).await;
    enable(&h, "admin").await;
    let run = super::memory::complete(&h, &cookie, conv, "我准备尝试新方案").await;
    tasks::discovery::process_one(&h.state).await.unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM followups WHERE source_run_id=$1 AND kind='checkin'",
    )
    .bind(run)
    .fetch_one(&h.state.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    sqlx::query("UPDATE followup_discovery SET status='queued',available_at=now() WHERE run_id=$1")
        .bind(run)
        .execute(&h.state.pool)
        .await
        .unwrap();
    tasks::discovery::process_one(&h.state).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM followups")
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        1
    );
    h.close().await;
}

// 验证跨午夜静默和 DST 歧义解析，使用真实时区规则；不依赖服务器实际所在地。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn timezone_and_quiet_hours() {
    let h = Harness::new().await;
    let prefs = tasks::preferences(&h.state, "admin").await.unwrap();
    let night = tasks::policy::parse_time("2026-10-01T23:00", "Asia/Shanghai").unwrap();
    assert!(tasks::policy::is_quiet(&prefs, night));
    assert!(!tasks::policy::is_quiet(
        &prefs,
        tasks::policy::next_awake(&prefs, night)
    ));
    assert!(tasks::policy::parse_time("2026-11-01T01:30", "America/New_York").is_err());
    assert!(tasks::policy::parse_time("2026-03-08T02:30", "America/New_York").is_err());
    h.close().await;
}

// 验证同一 owner 以外不可查询/改期/读通知，以及授权撤销后的已排期消息不发送；不模拟公网认证。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn identity_isolation_and_revocation() {
    let h = Harness::new().await;
    let admin = h.login().await;
    let (_, _, link) = h
        .request(
            "POST",
            "/api/admin/links",
            Some(&admin),
            json!({"label":"跟进夹具","expires_in_seconds":3600}),
        )
        .await;
    let token = link["url"]
        .as_str()
        .unwrap()
        .split("#token=")
        .nth(1)
        .unwrap();
    let (_, guest, _) = h
        .request("POST", "/api/auth/exchange", None, json!({"token":token}))
        .await;
    let guest = guest.unwrap();
    let (_, _, identity) = h.request("GET", "/api/me", Some(&guest), json!({})).await;
    let owner = identity["identity"]["owner"].as_str().unwrap();
    let id = create(&h, owner, None, "reminder", "访客私有事项").await;
    let (_, _, list) = h
        .request("GET", "/api/followups", Some(&admin), json!({}))
        .await;
    assert!(list["items"].as_array().unwrap().is_empty());
    assert_eq!(
        h.request(
            "PUT",
            &format!("/api/followups/{id}"),
            Some(&admin),
            json!({"version":1,"status":"cancelled"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let own = create(&h, "admin", None, "reminder", "管理员事项").await;
    due(&h, own).await;
    scheduler::process_one(&h.state, "reminder").await.unwrap();
    let (_, _, notices) = h
        .request("GET", "/api/notifications", Some(&admin), json!({}))
        .await;
    let notice = &notices["items"][0];
    assert_eq!(
        h.request(
            "POST",
            &format!("/api/notifications/{}/read", notice["id"]),
            Some(&guest),
            json!({})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE grants SET revoked_at=now()")
        .execute(&h.state.pool)
        .await
        .unwrap();
    due(&h, id).await;
    scheduler::process_one(&h.state, "reminder").await.unwrap();
    assert_eq!(get(&h, id).await.status, "cancelled");
    h.close().await;
}

// 验证发送前按原文哈希发现文件直接修正；不承诺对外部编辑工具提供跨进程事务锁。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn direct_memory_edit_invalidates_reminder() {
    let h = super::memory::setup(false).await;
    let cookie = h.login().await;
    let entry = super::memory::create(&h, &cookie, "方案", "原始方案正文", "project").await;
    let input = Create {
        communication: None,
        idempotency_key: Uuid::new_v4(),
        conversation_id: None,
        kind: "reminder".into(),
        topic: "原始方案正文".into(),
        due_at: (Utc::now() + Delta::hours(1)).to_rfc3339(),
        expires_at: None,
        memory_ids: vec![entry.id],
    };
    let result = tasks::create(&h.state, "admin", &input, None)
        .await
        .unwrap();
    let id = Uuid::parse_str(result["id"].as_str().unwrap()).unwrap();
    let path = h
        .state
        .config
        .memory
        .as_ref()
        .unwrap()
        .directory
        .join(auth::hash("admin"))
        .join(format!("{}.md", entry.id));
    let content = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, content.replace("原始方案正文", "已经换了方案")).unwrap();
    due(&h, id).await;
    scheduler::process_one(&h.state, "reminder").await.unwrap();
    assert_eq!(get(&h, id).await.error.as_deref(), Some("memory_changed"));
    assert_eq!(get(&h, id).await.status, "cancelled");
    h.close().await;
}

// 验证后台发现不会把遗忘边界以前的内容提交给模型，关闭回访也会废弃在途判断；不验证真实提取质量。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn boundary_and_opt_out_fence_background_work() {
    let h = super::memory::setup(false).await;
    let cookie = h.login().await;
    let conv = h.conversation(&cookie).await;
    enable(&h, "admin").await;
    super::memory::complete(&h, &cookie, conv, "我准备尝试新方案").await;
    let entry = super::memory::create(&h, &cookie, "背景", "保存明确的新事实", "profile").await;
    h.requests.lock().unwrap().clear();
    tasks::discovery::process_one(&h.state).await.unwrap();
    assert!(h.requests.lock().unwrap().is_empty());
    // 消除用户活动静默，仅用于触发模型竞态路径。
    sqlx::query("UPDATE runs SET created_at=now()-interval '1 hour'")
        .execute(&h.state.pool)
        .await
        .unwrap();
    let id = create(&h, "admin", Some(conv), "checkin", "新的事项").await;
    due(&h, id).await;
    let state = h.state.clone();
    let work = tokio::spawn(async move { scheduler::process_one(&state, "checkin").await });
    wait_model(&h).await;
    let mut prefs = tasks::preferences(&h.state, "admin").await.unwrap();
    prefs.enabled = false;
    tasks::save_preferences(&h.state, "admin", &prefs)
        .await
        .unwrap();
    work.await.unwrap().unwrap();
    assert_eq!(get(&h, id).await.status, "cancelled");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM messages WHERE kind='followup'")
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        0
    );
    assert!(
        orbit_server::memory::list(&h.state, "admin")
            .await
            .unwrap()
            .iter()
            .any(|e| e.id == entry.id)
    );
    h.close().await;
}

// 验证无有效模型决策时有界重试，以及确定完成时不发通知；不把夹具决定视为真实完成证据。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn model_failure_and_completion_are_bounded() {
    let h = Harness::new().await;
    enable(&h, "admin").await;
    let id = create(&h, "admin", None, "checkin", "夹具错误").await;
    for _ in 0..3 {
        due(&h, id).await;
        scheduler::process_one(&h.state, "checkin").await.unwrap();
    }
    assert_eq!(get(&h, id).await.status, "failed");
    let complete = create(&h, "admin", None, "checkin", "夹具完成").await;
    due(&h, complete).await;
    scheduler::process_one(&h.state, "checkin").await.unwrap();
    assert_eq!(get(&h, complete).await.status, "completed");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM messages WHERE kind='followup'")
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        0
    );
    h.close().await;
}

// 验证队列延迟超过活动静默后仍识别新用户内容；夹具仅验证协议围栏，不访问飞书公网。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn delayed_delivery_rechecks_observed_context() {
    let h = Harness::new().await;
    enable(&h, "feishu:ou_allowed").await;
    let conv = Uuid::new_v4();
    sqlx::query("INSERT INTO conversations(id,owner,channel,title) VALUES($1,'feishu:ou_allowed','feishu','延迟夹具')").bind(conv).execute(&h.state.pool).await.unwrap();
    let id = create(&h, "feishu:ou_allowed", Some(conv), "checkin", "新方案").await;
    due(&h, id).await;
    scheduler::process_one(&h.state, "checkin").await.unwrap();
    assert_eq!(get(&h, id).await.status, "queued");
    // 模拟在排队后产生、且已经过去一小时的用户轮次。
    sqlx::query("INSERT INTO runs(id,conversation_id,input,idempotency_key,status,created_at,batch_id) VALUES($1,$2,'方案已经改变',$3,'completed',now()-interval '1 hour',$1)").bind(Uuid::new_v4()).bind(conv).bind(Uuid::new_v4()).execute(&h.state.pool).await.unwrap();
    orbit_server::feishu::deliver_one(&h.state).await.unwrap();
    assert_eq!(get(&h, id).await.status, "scheduled");
    assert_eq!(get(&h, id).await.version, 2);
    assert!(
        !h.requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.get("receive_id").is_some())
    );
    h.close().await;
}

// 验证后续聊天收到主动消息背景且未创建伪用户轮次；模型只回显资料，不验证真实指代消解质量。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn proactive_context_continues_with_tools_disabled() {
    let mut h = Harness::new().await;
    let mut config = (*h.state.config).clone();
    config.model.tools_enabled = false;
    h.state = AppState::new(config, h.state.pool.clone()).unwrap();
    h.app = router(h.state.clone());
    let cookie = h.login().await;
    let conv = h.conversation(&cookie).await;
    let id = create(&h, "admin", Some(conv), "reminder", "交材料").await;
    due(&h, id).await;
    scheduler::process_one(&h.state, "reminder").await.unwrap();
    super::memory::complete(&h, &cookie, conv, "这件事已经做完了").await;
    {
        let requests = h.requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .any(|r| r["messages"].as_array().unwrap().iter().any(|m| {
                    m["content"].as_str().is_some_and(|s| {
                        s.contains("followup_state") && s.contains("提醒你：交材料")
                    })
                }))
        );
        assert!(requests.iter().all(|r| r.get("tools").is_none()));
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM runs")
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        1
    );
    h.close().await;
}

// 验证模型工具协议经过 runtime、宿主和生产 worker 后真正持久化；不验证真实模型识别日期的准确率。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn chat_tool_roundtrip_persists_reminder() {
    let h = Harness::new().await;
    let cookie = h.login().await;
    let conv = h.conversation(&cookie).await;
    let run = super::memory::complete(&h, &cookie, conv, "夹具提醒：明天交材料").await;
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM followups WHERE source_run_id=$1 AND kind='reminder' AND topic='交材料' AND status='scheduled'").bind(run).fetch_one(&h.state.pool).await.unwrap();
    assert_eq!(count, 1);
    {
        let requests = h.requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .any(|r| r["tools"].as_array().is_some_and(|tools| tools
                    .iter()
                    .any(|t| t["function"]["name"] == "followup_create")))
        );
        assert!(
            requests.iter().any(
                |r| r["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|m| m["role"] == "tool"
                        && m["content"]
                            .as_str()
                            .is_some_and(|s| s.contains("scheduled")))
            )
        );
    }
    h.close().await;
}
