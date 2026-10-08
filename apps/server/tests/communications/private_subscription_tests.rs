use super::*;

/// 只推进测试发现队列，不等待生产的十分钟轮询间隔。
async fn due_private(h: &Harness) {
    sqlx::query("UPDATE communication_connections SET next_discovery=now()")
        .execute(&h.state.pool)
        .await
        .unwrap();
}

/// 小型多类型候选页用于证明类型筛选不能仅依赖飞书请求参数。
fn page(items: Value, more: bool, cursor: &str) -> Value {
    json!({"code":0,"data":{"items":items,"has_more":more,"page_token":cursor}})
}

// 验证默认开启、分页只添加 p2p、历史边界、幂等及暂停/移除优先；上游候选来自夹具，不代表真实租户覆盖率。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn private_discovery_respects_scope_and_manual_controls() {
    let (h, fixture, server) = setup().await;
    fixture.lock().unwrap().private_pages = vec![
        page(
            json!([
                {"chat_id":"oc_fixture","name":"测试私聊","chat_mode":"p2p"},
                {"chat_id":"oc_group","name":"普通群","chat_mode":"group"},
                {"chat_id":"oc_topic","name":"话题群","chat_mode":"topic"},
                {"chat_id":"oc_unknown","name":"缺少类型"},
                {"chat_id":"oc_removed","name":"已排除","chat_mode":"p2p"}
            ]),
            true,
            "page-1",
        ),
        page(
            json!([{ "chat_id":"oc_second","name":"第二页私聊","chat_mode":"p2p" }]),
            false,
            "",
        ),
    ];
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let (_, _, status) = h
        .request(
            "GET",
            "/api/communications/status",
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(status["connection"]["auto_subscribe_private"], true);
    assert_eq!(status["connection"]["auto_subscribe"], false);
    sqlx::query("INSERT INTO communication_exclusions(owner,chat_id) VALUES('admin','oc_removed')")
        .execute(&h.state.pool)
        .await
        .unwrap();
    communications::private_subscription::step(&h.state)
        .await
        .unwrap();
    due_private(&h).await;
    communications::private_subscription::step(&h.state)
        .await
        .unwrap();
    let rows: Vec<(Uuid, String, i64, bool)> = sqlx::query_as(
        "SELECT id,chat_id,start_at,enabled FROM communication_sources ORDER BY chat_id",
    )
    .fetch_all(&h.state.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "oc_fixture");
    assert_eq!(rows[1].1, "oc_second");
    let since: i64 =
        sqlx::query_scalar("SELECT private_subscription_since FROM communication_connections")
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert!(rows.iter().all(|row| row.2 == since && row.3));
    let history: i64 = sqlx::query_scalar("SELECT count(*) FROM communication_history_jobs")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(history, 0);
    let id = rows[0].0;
    assert_eq!(
        h.request(
            "PUT",
            &format!("/api/communications/sources/{id}"),
            Some(&cookie),
            json!({"enabled":false,"version":1})
        )
        .await
        .0,
        StatusCode::OK
    );
    due_private(&h).await;
    communications::private_subscription::step(&h.state)
        .await
        .unwrap();
    let state: (bool, i64) =
        sqlx::query_as("SELECT enabled,version FROM communication_sources WHERE id=$1")
            .bind(id)
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert_eq!(state, (false, 2));
    assert_eq!(
        h.request(
            "POST",
            "/api/communications/sources/remove",
            Some(&cookie),
            json!({"selection":{"scope":"one","id":id,"version":2},"delete_documents":true})
        )
        .await
        .0,
        StatusCode::OK
    );
    for _ in 0..3 {
        due_private(&h).await;
        communications::private_subscription::step(&h.state)
            .await
            .unwrap();
    }
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM communication_sources WHERE chat_id='oc_fixture')",
    )
    .fetch_one(&h.state.pool)
    .await
    .unwrap();
    assert!(!exists);
    // 显式重新订阅才解除排除；后台发现不能代替这个用户动作。
    assert_eq!(
        h.request(
            "POST",
            "/api/communications/sources",
            Some(&cookie),
            json!({"chat_id":"oc_fixture","label":"恢复私聊"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let excluded: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM communication_exclusions WHERE chat_id='oc_fixture')",
    )
    .fetch_one(&h.state.pool)
    .await
    .unwrap();
    assert!(!excluded);
    server.abort();
    h.close().await;
}

// 验证发现失败退避和删除后重连的在途围栏；不等待真实网络超时，也不发送真实飞书消息。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn private_discovery_retries_and_fences_reconnection() {
    let (h, fixture, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    fixture.lock().unwrap().private_pages = vec![json!({"code":999,"msg":"fixture failure"})];
    communications::private_subscription::step(&h.state)
        .await
        .unwrap();
    let failure: (Option<String>, bool) = sqlx::query_as(
        "SELECT discovery_error,next_discovery>now() FROM communication_connections",
    )
    .fetch_one(&h.state.pool)
    .await
    .unwrap();
    assert_eq!(
        failure,
        (Some("communication_provider_rejected".into()), true)
    );
    let gate = Arc::new(MessageGate {
        arrived: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    {
        let mut f = fixture.lock().unwrap();
        f.private_pages = vec![page(
            json!([{ "chat_id":"oc_stale","name":"旧请求","chat_mode":"p2p" }]),
            false,
            "",
        )];
        f.private_gate = Some(gate.clone());
    }
    due_private(&h).await;
    let state = h.state.clone();
    let request =
        tokio::spawn(async move { communications::private_subscription::step(&state).await });
    tokio::time::timeout(Duration::from_secs(3), gate.arrived.notified())
        .await
        .unwrap();
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
    connect(&h, &cookie).await;
    gate.release.notify_one();
    request.await.unwrap().unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM communication_sources")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    fixture.lock().unwrap().private_gate = None;
    communications::private_subscription::step(&h.state)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM communication_sources")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    server.abort();
    h.close().await;
}
