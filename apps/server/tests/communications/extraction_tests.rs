use super::*;

/// 一份待升级群聊资料及其隔离测试资源，使用命名字段避免混淆来源和文档标识。
struct OldGroup {
    /// 真实数据库与应用路由。
    h: Harness,
    /// 本地飞书协议的可控状态。
    fixture: Arc<Mutex<Fixture>>,
    /// 测试结束时关闭的协议服务器。
    server: tokio::task::JoinHandle<()>,
    /// 通过管理员登录获得的会话。
    cookie: String,
    /// 可暂停和恢复的来源。
    source: Uuid,
    /// 已隔离、等待重新提取的文档。
    document: Uuid,
}

/// 重放已选时间范围，模拟历史导入或每日审计返回同一批消息。
async fn replay(h: &Harness) {
    sqlx::query("UPDATE communication_sources SET window_start=NULL,window_end=NULL,page_token='',audit_at=now()-interval '1 day',next_sync=now()")
        .execute(&h.state.pool).await.unwrap();
    for _ in 0..2 {
        due(h).await;
        communications::sync::step(&h.state).await.unwrap();
    }
}

/// 通过生产详情接口检查资料版本和原文，避免只验证内部合并结果。
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

/// 使用真实版本更新接口切换来源状态；旧响应必须保留领取时的版本。
async fn enabled(h: &Harness, cookie: &str, source: Uuid, value: bool) {
    let version: i64 = sqlx::query_scalar("SELECT version FROM communication_sources WHERE id=$1")
        .bind(source)
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    let (status, _, response) = h
        .request(
            "PUT",
            &format!("/api/communications/sources/{source}"),
            Some(cookie),
            json!({"version":version,"enabled":value}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
}

/// 单页群聊仅含本人消息，便于精确核对重处理读取次数；不连接真实飞书。
async fn old_group() -> OldGroup {
    let (h, fixture, server) = setup().await;
    fixture.lock().unwrap().group = true;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    due(&h).await;
    communications::sync::step(&h.state).await.unwrap();
    let document: Uuid = sqlx::query_scalar("SELECT id FROM communication_documents LIMIT 1")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE communication_documents SET extraction_version=0")
        .execute(&h.state.pool)
        .await
        .unwrap();
    OldGroup {
        h,
        fixture,
        server,
        cookie,
        source,
        document,
    }
}

// 验证启动时隔离旧上下文，之后的 UTC 归档、逐日修正和重启检查不取消新任务或推进边界；不评价模型语义。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn upgrade_preserves_new_chat_after_startup_boundary() {
    for regroup in [false, true] {
        let OldGroup {
            h,
            fixture,
            server,
            cookie,
            ..
        } = old_group().await;
        let old_conversation = h.conversation(&cookie).await;
        let old_run = h
            .send(&cookie, old_conversation, "升级前的问题", Uuid::new_v4())
            .await;
        // 覆盖已应用 0011 的安装：新迁移必须重新安排一次启动清理。
        sqlx::query(include_str!(
            "../../migrations/0012_communication_scope_boundary.sql"
        ))
        .execute(&h.state.pool)
        .await
        .unwrap();
        let reads = fixture.lock().unwrap().reads;
        communications::sync::reprocess_step(&h.state)
            .await
            .unwrap();
        assert_eq!(
            fixture.lock().unwrap().reads,
            reads,
            "启动清理前不能领取旧资料"
        );
        communications::sync::initialize_scope(&h.state)
            .await
            .unwrap();
        let old_status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id=$1")
            .bind(old_run)
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
        assert_eq!(old_status, "cancelled");
        let boundary: i64 =
            sqlx::query_scalar("SELECT forgotten_through FROM memory_owners WHERE owner='admin'")
                .fetch_one(&h.state.pool)
                .await
                .unwrap();
        let running_conversation = h.conversation(&cookie).await;
        let running = h
            .send(
                &cookie,
                running_conversation,
                "升级后的新问题",
                Uuid::new_v4(),
            )
            .await;
        sqlx::query("UPDATE runs SET available_at=now() WHERE id=$1")
            .bind(running)
            .execute(&h.state.pool)
            .await
            .unwrap();
        assert_eq!(worker::claim(&h.state).await.unwrap().unwrap().id, running);
        let queued_conversation = h.conversation(&cookie).await;
        let queued = h
            .send(&cookie, queued_conversation, "另一条新问题", Uuid::new_v4())
            .await;
        if regroup {
            // 旧 UTC 日期与消息的北京时间日期不一致，触发真实归档迁移路径。
            sqlx::query("UPDATE communication_documents SET day='2000-01-01'")
                .execute(&h.state.pool)
                .await
                .unwrap();
            sqlx::query("UPDATE communication_sources SET day_timezone='UTC',next_sync=now()+interval '1 day'")
                .execute(&h.state.pool).await.unwrap();
            communications::sync::step(&h.state).await.unwrap();
        }
        // 上游确实修改正文，保证同时覆盖 commit_day 的 corrected 分支。
        fixture.lock().unwrap().edited = true;
        communications::sync::reprocess_step(&h.state)
            .await
            .unwrap();
        communications::sync::initialize_scope(&h.state)
            .await
            .unwrap();
        let statuses: Vec<String> =
            sqlx::query_scalar("SELECT status FROM runs WHERE id=ANY($1) ORDER BY seq")
                .bind(vec![running, queued])
                .fetch_all(&h.state.pool)
                .await
                .unwrap();
        assert_eq!(statuses, vec!["running", "queued"]);
        let after: i64 =
            sqlx::query_scalar("SELECT forgotten_through FROM memory_owners WHERE owner='admin'")
                .fetch_one(&h.state.pool)
                .await
                .unwrap();
        assert_eq!(after, boundary);
        let id: Uuid = sqlx::query_scalar("SELECT id FROM communication_documents LIMIT 1")
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
        let value = detail(&h, &cookie, id).await;
        assert_eq!(value["document"]["extraction_version"], 1);
        assert!(
            value["messages"][0]["text"]
                .as_str()
                .unwrap()
                .contains("取消")
        );
        server.abort();
        h.close().await;
    }
}

// 验证暂停时不读取、恢复后可升级；使用本地协议计数，不保证撤回已开始的真实 HTTP 请求。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn paused_sources_are_not_reprocessed_until_resumed() {
    let OldGroup {
        h,
        fixture,
        server,
        cookie,
        source,
        document,
    } = old_group().await;
    enabled(&h, &cookie, source, false).await;
    let reads = fixture.lock().unwrap().reads;
    communications::sync::reprocess_step(&h.state)
        .await
        .unwrap();
    assert_eq!(fixture.lock().unwrap().reads, reads);
    assert_eq!(detail(&h, &cookie, document).await["processing"], true);
    enabled(&h, &cookie, source, true).await;
    communications::sync::reprocess_step(&h.state)
        .await
        .unwrap();
    assert_eq!(fixture.lock().unwrap().reads, reads + 1);
    assert_eq!(
        detail(&h, &cookie, document).await["document"]["extraction_version"],
        1
    );
    server.abort();
    h.close().await;
}

// 用通知屏障验证暂停及立即恢复均拒绝旧重处理响应；不以固定休眠模拟真实网络时序。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn paused_source_fences_inflight_reprocessing() {
    for resume in [false, true] {
        let OldGroup {
            h,
            fixture,
            server,
            cookie,
            source,
            document,
        } = old_group().await;
        let before = detail(&h, &cookie, document).await["document"].clone();
        let gate = Arc::new(MessageGate {
            arrived: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        fixture.lock().unwrap().message_gate = Some(gate.clone());
        let state = h.state.clone();
        let task = tokio::spawn(async move { communications::sync::reprocess_step(&state).await });
        tokio::time::timeout(Duration::from_secs(5), gate.arrived.notified())
            .await
            .unwrap();
        enabled(&h, &cookie, source, false).await;
        if resume {
            enabled(&h, &cookie, source, true).await;
        }
        gate.release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(detail(&h, &cookie, document).await["document"], before);
        server.abort();
        h.close().await;
    }
}

// 验证乱序的旧无关版本不删除新版消息、不取消提醒；最新的无关版本仍移除记录并取消依赖。不验证上游实际乱序频率。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn unrelated_edits_obey_message_versions() {
    let (h, fixture, server) = setup().await;
    {
        let mut f = fixture.lock().unwrap();
        f.group = true;
        f.mention_me = true;
        f.edited = true;
    }
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    add(&h, &cookie).await;
    let doc = import(&h, &cookie).await;
    let id = Uuid::parse_str(doc["id"].as_str().unwrap()).unwrap();
    let before = detail(&h, &cookie, id).await;
    assert_eq!(before["total"], 2);
    let item = before["summary"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .position(|item| item["message_id"] == "om_other")
        .unwrap();
    let (status, _, task) = h
        .request(
            "POST",
            "/api/followups",
            Some(&cookie),
            json!({
                "idempotency_key":Uuid::new_v4(),"kind":"reminder","topic":"核对材料进度",
                "due_at":(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339(),
                "communication":{"document_id":id,"version":doc["version"],"item":item}
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{task}");
    let task_id = Uuid::parse_str(task["id"].as_str().unwrap()).unwrap();
    {
        let mut f = fixture.lock().unwrap();
        f.edited = false;
        f.mention_me = false;
    }
    replay(&h).await;
    let after = detail(&h, &cookie, id).await;
    assert_eq!(after, before, "旧响应不能改变原文、摘要或版本");
    let status: String = sqlx::query_scalar("SELECT status FROM followups WHERE id=$1")
        .bind(task_id)
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(status, "scheduled");
    fixture.lock().unwrap().edited = true;
    replay(&h).await;
    let after = detail(&h, &cookie, id).await;
    assert_eq!(after["total"], 1, "当前版本不再关联时仍必须移除");
    let status: String = sqlx::query_scalar("SELECT status FROM followups WHERE id=$1")
        .bind(task_id)
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(status, "cancelled");
    server.abort();
    h.close().await;
}
