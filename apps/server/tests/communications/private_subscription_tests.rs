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

// 验证无排除历史来源的恢复、补采边界、文件复用与幂等；使用本地飞书/模型夹具，不代表线上遗漏已补齐。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn private_discovery_recovers_unexcluded_history_and_resumes_sync() {
    let (h, fixture, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    let original = import(&h, &cookie).await;
    let since = fixture.lock().unwrap().base / 1000 - 60;
    sqlx::query(
        "UPDATE communication_connections SET private_subscription_since=$1,next_discovery=now()",
    )
    .bind(since)
    .execute(&h.state.pool)
    .await
    .unwrap();
    // 模拟没有退出记录的旧来源；残留分页不能被恢复后的采集继续使用。
    sqlx::query("UPDATE communication_sources SET subscribed=false,enabled=false,version=2,watermark=$2+3600,page_token='stale',window_start=$2,window_end=$2+3600,error='old_failure',next_sync=now()+interval '1 day' WHERE id=$1")
        .bind(source).bind(since).execute(&h.state.pool).await.unwrap();
    sqlx::query("INSERT INTO communication_history_jobs(id,source_id,start_at,end_at,snapshot_end,status) VALUES($1,$2,1,2,2,'cancelled')")
        .bind(Uuid::new_v4()).bind(source).execute(&h.state.pool).await.unwrap();
    communications::private_subscription::step(&h.state)
        .await
        .unwrap();
    let recovered: (Uuid, bool, bool, i64, i64, i64, String, bool, bool) = sqlx::query_as(
        "SELECT id,subscribed,enabled,version,start_at,watermark,page_token,window_start IS NULL AND window_end IS NULL AND error IS NULL,next_sync<=now() FROM communication_sources WHERE chat_id='oc_fixture'",
    ).fetch_one(&h.state.pool).await.unwrap();
    assert_eq!(
        recovered,
        (
            source,
            true,
            true,
            3,
            since,
            since,
            String::new(),
            true,
            true
        )
    );
    let raw_hash: String =
        sqlx::query_scalar("SELECT raw_hash FROM communication_documents WHERE source_id=$1")
            .bind(source)
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert_eq!(raw_hash, original["raw_hash"].as_str().unwrap());
    due_private(&h).await;
    communications::private_subscription::step(&h.state)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT version FROM communication_sources WHERE id=$1")
            .bind(source)
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        3
    );
    fixture.lock().unwrap().edited = true;
    let updated = import(&h, &cookie).await;
    assert_eq!(updated["id"], original["id"]);
    assert_ne!(updated["raw_hash"], original["raw_hash"]);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT status FROM communication_history_jobs WHERE source_id=$1"
        )
        .bind(source)
        .fetch_one(&h.state.pool)
        .await
        .unwrap(),
        "cancelled"
    );
    server.abort();
    h.close().await;
}

// 验证恢复只针对未排除私聊，保留暂停、启用、明确移除及更晚起点；不推断旧排除记录的操作者或原因。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn private_discovery_preserves_opt_outs_and_later_boundaries() {
    let (h, fixture, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let since: i64 =
        sqlx::query_scalar("SELECT private_subscription_since FROM communication_connections")
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    let cases = [
        ("oc_paused", true, false, "p2p", false),
        ("oc_enabled", true, true, "p2p", false),
        ("oc_excluded", false, false, "p2p", true),
        ("oc_group", false, false, "group", false),
        ("oc_unknown", false, false, "unknown", false),
        ("oc_later", false, false, "p2p", false),
    ];
    let mut items = vec![];
    for (chat, subscribed, enabled, mode, excluded) in cases {
        sqlx::query("INSERT INTO communication_sources(id,owner,chat_id,label,subscribed,enabled,start_at,watermark,day_timezone) VALUES($1,'admin',$2,$2,$3,$4,$5,$5,'Asia/Shanghai')")
            .bind(Uuid::new_v4()).bind(chat).bind(subscribed).bind(enabled).bind(since+60)
            .execute(&h.state.pool).await.unwrap();
        if excluded {
            sqlx::query("INSERT INTO communication_exclusions(owner,chat_id) VALUES('admin',$1)")
                .bind(chat)
                .execute(&h.state.pool)
                .await
                .unwrap();
        }
        items.push(json!({"chat_id":chat,"name":chat,"chat_mode":mode}));
    }
    fixture.lock().unwrap().private_pages = vec![page(json!(items), false, "")];
    communications::private_subscription::step(&h.state)
        .await
        .unwrap();
    for (chat, subscribed, enabled, _, _) in cases {
        let row: (bool, bool, i64, i64, i64) = sqlx::query_as("SELECT subscribed,enabled,version,start_at,watermark FROM communication_sources WHERE chat_id=$1")
            .bind(chat).fetch_one(&h.state.pool).await.unwrap();
        let expected = if chat == "oc_later" {
            (true, true, 2, since + 60, since + 60)
        } else {
            (subscribed, enabled, 1, since + 60, since + 60)
        };
        assert_eq!(row, expected, "{chat}");
    }
    server.abort();
    h.close().await;
}
