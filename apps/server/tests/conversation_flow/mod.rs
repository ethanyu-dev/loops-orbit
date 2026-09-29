use super::*;

/// 等待真实数据库出现尚未完成的回复片段，避免固定休眠产生偶然通过。
async fn partial(h: &Harness, run: Uuid) -> String {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let (status, text): (String, String) =
                sqlx::query_as("SELECT status,partial_content FROM runs WHERE id=$1")
                    .bind(run)
                    .fetch_one(&h.state.pool)
                    .await
                    .unwrap();
            if status == "running" && !text.is_empty() {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("没有观察到流式片段")
}

/// 通过生产取消接口停止指定执行，不使用直接改表代替用户操作。
async fn cancel(h: &Harness, cookie: &str, conversation: Uuid, run: Uuid) -> Value {
    let (status, _, body) = h
        .request(
            "POST",
            &format!("/api/conversations/{conversation}/runs/{run}/cancel"),
            Some(cookie),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    body
}

// 验证连续输入合并、幂等重试、流中改口及旧结果围栏；不验证真实模型对自然语言的判断质量。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn new_input_supersedes_old_generation() {
    let h = Harness::new().await;
    let admin = h.login().await;
    let id = h.conversation(&admin).await;
    let key = Uuid::new_v4();
    let first = h.send(&admin, id, "帮我选周末行程", key).await;
    let second = h.send(&admin, id, "只考虑上海", Uuid::new_v4()).await;
    assert_eq!(first, h.send(&admin, id, "帮我选周末行程", key).await);
    assert!(worker::claim(&h.state).await.unwrap().is_none());
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(worker::run_worker(h.state.clone(), receiver));
    wait_for(&h.state, "completed", 1).await;
    {
        let requests = h.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["messages"][1]["content"], "帮我选周末行程");
        assert_eq!(requests[0]["messages"][2]["content"], "只考虑上海");
    }
    let slow = h.send(&admin, id, "slow-stream old", Uuid::new_v4()).await;
    assert_eq!(partial(&h, slow).await, "先生成的片段");
    let latest = h.send(&admin, id, "改成在家休息", Uuid::new_v4()).await;
    assert_eq!(cancel(&h, &admin, id, slow).await["cancelled"], false);
    wait_for(&h.state, "completed", 2).await;
    let stale: (String, String, bool) = sqlx::query_as("SELECT status,partial_content,EXISTS(SELECT 1 FROM messages WHERE run_id=$1 AND role='assistant') FROM runs WHERE id=$1")
        .bind(slow).fetch_one(&h.state.pool).await.unwrap();
    assert_eq!(stale, ("superseded".into(), String::new(), false));
    let completed: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM runs WHERE status='completed' ORDER BY seq")
            .fetch_all(&h.state.pool)
            .await
            .unwrap();
    assert_eq!(completed, vec![second, latest]);
    stop.send(true).unwrap();
    task.await.unwrap();
    h.close().await;
}

// 验证取消整个未完成输入批次、重复取消、取消权限和后续上下文；不保证上游收到断连后立即停止计费。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn cancellation_discards_batch_and_partial() {
    let h = Harness::new().await;
    let admin = h.login().await;
    let id = h.conversation(&admin).await;
    h.send(&admin, id, "不要保留的旧请求", Uuid::new_v4()).await;
    let slow = h
        .send(&admin, id, "slow-stream cancel", Uuid::new_v4())
        .await;
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(worker::run_worker(h.state.clone(), receiver));
    partial(&h, slow).await;
    assert_eq!(
        h.request(
            "POST",
            &format!("/api/conversations/{id}/runs/{slow}/cancel"),
            None,
            json!({})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(cancel(&h, &admin, id, slow).await["cancelled"], true);
    assert_eq!(cancel(&h, &admin, id, slow).await["cancelled"], false);
    wait_for(&h.state, "cancelled", 2).await;
    h.send(&admin, id, "新的独立话题", Uuid::new_v4()).await;
    wait_for(&h.state, "completed", 1).await;
    {
        let requests = h.requests.lock().unwrap();
        let messages = requests.last().unwrap()["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["content"], "新的独立话题");
    }
    let fragments: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM runs WHERE status='cancelled' AND partial_content<>''",
    )
    .fetch_one(&h.state.pool)
    .await
    .unwrap();
    assert_eq!(fragments, 0);
    stop.send(true).unwrap();
    task.await.unwrap();
    h.close().await;
}

// 验证流式快照在新详情请求中恢复并原子转为完整消息；模型为延迟 SSE 夹具，不代表真实浏览器刷新验收。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn streaming_snapshot_survives_detail_reload() {
    let h = Harness::new().await;
    let admin = h.login().await;
    let id = h.conversation(&admin).await;
    let run = h
        .send(&admin, id, "slow-stream complete", Uuid::new_v4())
        .await;
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(worker::run_worker(h.state.clone(), receiver));
    partial(&h, run).await;
    let (_, _, detail) = h
        .request(
            "GET",
            &format!("/api/conversations/{id}"),
            Some(&admin),
            json!({}),
        )
        .await;
    assert_eq!(detail["runs"][0]["partial_content"], "先生成的片段");
    assert_eq!(detail["messages"].as_array().unwrap().len(), 1);
    wait_for(&h.state, "completed", 1).await;
    let (_, _, detail) = h
        .request(
            "GET",
            &format!("/api/conversations/{id}"),
            Some(&admin),
            json!({}),
        )
        .await;
    assert_eq!(detail["runs"][0]["partial_content"], "");
    assert_eq!(
        detail["messages"][1]["content"],
        "先生成的片段，后生成的内容"
    );
    stop.send(true).unwrap();
    task.await.unwrap();
    h.close().await;
}

/// 构造稳定历史用来跨过摘要阈值，避免把夹具数据描述成真实模型产出的事实。
async fn seed_history(h: &Harness, id: Uuid, first: &str) {
    for index in 0..20 {
        let run = Uuid::new_v4();
        let input = if index == 0 {
            first.to_owned()
        } else {
            format!("历史话题{index}")
        };
        sqlx::query("INSERT INTO runs(id,conversation_id,idempotency_key,input,status,batch_id) VALUES($1,$2,$3,$4,'completed',$1)")
            .bind(run).bind(id).bind(run.to_string()).bind(&input).execute(&h.state.pool).await.unwrap();
        for (role, content) in [("user", input.as_str()), ("assistant", "已经讨论")] {
            sqlx::query(
                "INSERT INTO messages(conversation_id,run_id,role,content) VALUES($1,$2,$3,$4)",
            )
            .bind(id)
            .bind(run)
            .bind(role)
            .bind(content)
            .execute(&h.state.pool)
            .await
            .unwrap();
        }
    }
}

// 验证早期信息经摘要进入后续请求、近期修正仍保留原文及会话隔离；不验证模型摘要的语义保真度。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn summary_preserves_older_context_and_recent_corrections() {
    let h = Harness::new().await;
    let admin = h.login().await;
    let id = h.conversation(&admin).await;
    let other = h.conversation(&admin).await;
    seed_history(&h, id, "预算两千，只考虑上海").await;
    seed_history(&h, other, "另一个会话的私密偏好").await;
    h.send(&admin, id, "预算改为一千，请接着讨论", Uuid::new_v4())
        .await;
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(worker::run_worker(h.state.clone(), receiver));
    wait_for(&h.state, "completed", 41).await;
    {
        let requests = h.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].get("tools").is_none());
        let messages = requests[1]["messages"].as_array().unwrap();
        assert!(
            messages[1]["content"]
                .as_str()
                .unwrap()
                .contains("预算两千")
        );
        assert_eq!(
            messages.last().unwrap()["content"],
            "预算改为一千，请接着讨论"
        );
        assert!(!requests[1].to_string().contains("另一个会话的私密偏好"));
    }
    let (summary, boundary): (String, i64) =
        sqlx::query_as("SELECT context_summary,summary_through FROM conversations WHERE id=$1")
            .bind(id)
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert!(summary.contains("只考虑上海"));
    assert!(boundary > 0);
    stop.send(true).unwrap();
    task.await.unwrap();
    h.close().await;
}

// 验证摘要失败不推进检查点、不静默丢弃旧历史；不模拟真实供应商所有故障类型。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn failed_summary_does_not_advance_checkpoint() {
    let h = Harness::new().await;
    let admin = h.login().await;
    let id = h.conversation(&admin).await;
    seed_history(&h, id, "summary-error").await;
    h.send(&admin, id, "继续", Uuid::new_v4()).await;
    sqlx::query("UPDATE runs SET available_at=now()")
        .execute(&h.state.pool)
        .await
        .unwrap();
    let job = worker::claim(&h.state).await.unwrap().unwrap();
    assert!(
        orbit_server::context::prepare(&h.state, &job)
            .await
            .is_err()
    );
    let checkpoint: i64 =
        sqlx::query_scalar("SELECT summary_through FROM conversations WHERE id=$1")
            .bind(id)
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert_eq!(checkpoint, 0);
    h.close().await;
}

// 超过近期轮数的未回答消息仍保留原文，取消后不能残留摘要；不验证真实模型对密集输入的理解。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn pending_batch_is_never_summarized() {
    let h = Harness::new().await;
    let admin = h.login().await;
    let id = h.conversation(&admin).await;
    let mut latest = Uuid::nil();
    for index in 0..16 {
        latest = h
            .send(&admin, id, &format!("补充条件{index}"), Uuid::new_v4())
            .await;
    }
    sqlx::query("UPDATE runs SET available_at=now()")
        .execute(&h.state.pool)
        .await
        .unwrap();
    let job = worker::claim(&h.state).await.unwrap().unwrap();
    let messages = orbit_server::context::prepare(&h.state, &job)
        .await
        .unwrap();
    assert_eq!(messages.len(), 16);
    assert_eq!(messages[0].content, "补充条件0");
    assert_eq!(messages[15].content, "补充条件15");
    assert!(h.requests.lock().unwrap().is_empty());
    cancel(&h, &admin, id, latest).await;
    let summary: String =
        sqlx::query_scalar("SELECT context_summary FROM conversations WHERE id=$1")
            .bind(id)
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert!(summary.is_empty());
    wait_for(&h.state, "cancelled", 16).await;
    h.close().await;
}

// 验证补充超限事务回滚后原执行仍有效，不会先取消再拒绝输入；不覆盖供应商 token 限制。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn rejected_supplement_keeps_existing_run() {
    let h = Harness::new().await;
    let admin = h.login().await;
    let id = h.conversation(&admin).await;
    h.send(&admin, id, &"甲".repeat(12_000), Uuid::new_v4())
        .await;
    let latest = h
        .send(&admin, id, &"乙".repeat(12_000), Uuid::new_v4())
        .await;
    let (status, _, result) = h
        .request(
            "POST",
            &format!("/api/conversations/{id}/messages"),
            Some(&admin),
            json!({"content":"丙","idempotency_key":Uuid::new_v4()}),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(result["error"], "pending_context_full");
    let active: Uuid = sqlx::query_scalar("SELECT id FROM runs WHERE status='queued'")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(active, latest);
    h.close().await;
}
