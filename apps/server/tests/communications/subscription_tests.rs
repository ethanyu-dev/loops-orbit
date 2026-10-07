use super::*;

// 验证默认不订阅、加载候选无副作用、显式订阅幂等；不验证真实飞书租户。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn manual_subscription_is_explicit() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let auto: bool = sqlx::query_scalar("SELECT auto_subscribe FROM communication_connections")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert!(!auto);
    assert_eq!(
        h.request("GET", "/api/communications/chats", Some(&cookie), json!({}))
            .await
            .0,
        StatusCode::OK
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM communication_sources")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    for _ in 0..2 {
        subscribe_fixture(&h, &cookie).await;
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM communication_sources")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    server.abort();
    h.close().await;
}

/// 测试夹具显式选择来源，不能依靠授权时自动订阅。
async fn subscribe_fixture(h: &Harness, cookie: &str) {
    assert_eq!(
        h.request(
            "POST",
            "/api/communications/sources",
            Some(cookie),
            json!({"chat_id":"oc_fixture","label":"测试沟通"})
        )
        .await
        .0,
        StatusCode::OK
    );
}

// 验证手选历史独立于增量水位、重复补录不增版本、错误日期被拒绝；不验证飞书实际历史覆盖范围。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn history_is_independent_and_idempotent() {
    let (h, fixture, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    subscribe_fixture(&h, &cookie).await;
    let (id, watermark): (Uuid, i64) =
        sqlx::query_as("SELECT id,watermark FROM communication_sources")
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    let day = chrono::DateTime::from_timestamp_millis(fixture.lock().unwrap().base)
        .unwrap()
        .with_timezone(&chrono_tz::Asia::Shanghai)
        .format("%Y-%m-%d")
        .to_string();
    let route = format!("/api/communications/sources/{id}/history");
    let body = json!({"start_date":day,"end_date":day});
    assert_eq!(
        h.request("POST", &route, Some(&cookie), body.clone())
            .await
            .0,
        StatusCode::OK
    );
    for _ in 0..2 {
        sqlx::query("UPDATE communication_history_jobs SET next_attempt=now()")
            .execute(&h.state.pool)
            .await
            .unwrap();
        communications::subscription::history_step(&h.state)
            .await
            .unwrap();
    }
    let (doc, version): (Uuid, i64) =
        sqlx::query_as("SELECT id,version FROM communication_documents")
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    let after: i64 = sqlx::query_scalar("SELECT watermark FROM communication_sources")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(after, watermark);
    assert_eq!(
        h.request("POST", &route, Some(&cookie), body).await.0,
        StatusCode::OK
    );
    for _ in 0..2 {
        sqlx::query("UPDATE communication_history_jobs SET next_attempt=now()")
            .execute(&h.state.pool)
            .await
            .unwrap();
        communications::subscription::history_step(&h.state)
            .await
            .unwrap();
    }
    let rows: Vec<(Uuid, i64)> = sqlx::query_as("SELECT id,version FROM communication_documents")
        .fetch_all(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(rows, vec![(doc, version)]);
    let (_, _, detail) = h
        .request(
            "GET",
            &format!("/api/communications/documents/{doc}"),
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(detail["total"], 2);
    assert_eq!(detail["messages"][1]["sender_name"], "小林");
    assert_eq!(
        h.request(
            "POST",
            &route,
            Some(&cookie),
            json!({"start_date":"2024-01-01","end_date":"2026-01-01"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    server.abort();
    h.close().await;
}

// 以真实文件构造旧 UTC 单日快照，验证迁移按北京时间拆日且不丢消息；不读取生产资料。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn utc_files_regroup_without_loss() {
    let (h, fixture, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    let base = fixture.lock().unwrap().base;
    let day = chrono::DateTime::from_timestamp_millis(base)
        .unwrap()
        .date_naive();
    let before = day
        .and_hms_opt(15, 59, 59)
        .unwrap()
        .and_utc()
        .timestamp_millis();
    let messages:Vec<Value>=[before,before+2000].into_iter().enumerate().map(|(i,time)|json!({"message_id":format!("om_migration_{i}"),"chat_id":"oc_fixture","sender_id":"ou_allowed","sender_id_type":"open_id","sender_type":"user","is_me":true,"create_time":time,"update_time":time,"message_type":"text","deleted":false,"text":"迁移测试","payload":null})).collect();
    let content = messages
        .iter()
        .map(|v| format!("{v}\n"))
        .collect::<String>();
    let hash = auth::hash(&content);
    let id = Uuid::new_v4();
    let directory = h
        .state
        .config
        .memory
        .as_ref()
        .unwrap()
        .directory
        .join("_communications")
        .join(source.to_string())
        .join(id.to_string());
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join(format!("{hash}.jsonl")), content).unwrap();
    sqlx::query(
        "INSERT INTO communication_documents(id,source_id,day,raw_hash) VALUES($1,$2,$3,$4)",
    )
    .bind(id)
    .bind(source)
    .bind(day.to_string())
    .bind(hash)
    .execute(&h.state.pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE communication_sources SET day_timezone='UTC',next_sync=now()+interval '1 day'",
    )
    .execute(&h.state.pool)
    .await
    .unwrap();
    communications::sync::step(&h.state).await.unwrap();
    let docs: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id,day FROM communication_documents ORDER BY day")
            .fetch_all(&h.state.pool)
            .await
            .unwrap();
    assert_eq!(docs.len(), 2);
    assert_eq!(docs[0].1, day.to_string());
    assert_eq!(docs[1].1, day.succ_opt().unwrap().to_string());
    for (id, _) in docs {
        let (_, _, v) = h
            .request(
                "GET",
                &format!("/api/communications/documents/{id}"),
                Some(&cookie),
                Value::Null,
            )
            .await;
        assert_eq!(v["total"], 1);
    }
    server.abort();
    h.close().await;
}

// 验证图片通过鉴权路由、模型收到真正图片块、解读可检索和遗忘清理；图片字节/解读均为协议夹具。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn image_resource_vision_and_forget() {
    let (h, fixture, server) = setup().await;
    fixture.lock().unwrap().image = true;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    let doc = import(&h, &cookie).await;
    let id = doc["id"].as_str().unwrap();
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(communications::sync::run(h.state.clone(), receiver));
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let ready: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM communication_images WHERE description IS NOT NULL)",
            )
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
    let (_, _, detail) = h
        .request(
            "GET",
            &format!("/api/communications/documents/{id}"),
            Some(&cookie),
            Value::Null,
        )
        .await;
    let image = &detail["messages"][0]["images"][0];
    assert!(image["description"].as_str().unwrap().contains("128"));
    let url = image["url"].as_str().unwrap();
    assert!(!url.contains("token"));
    assert_eq!(
        h.request("GET", url, None, Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        h.request("GET", url, Some(&cookie), Value::Null).await.0,
        StatusCode::OK
    );
    assert!(
        !communications::retrieve(&h.state, "admin", "等待付款")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        h.request(
            "DELETE",
            &format!("/api/communications/sources/{source}"),
            Some(&cookie),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        h.request("GET", url, Some(&cookie), Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM communication_images")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    server.abort();
    h.close().await;
}

// 验证当天历史区间的逻辑 ID 稳定、分页终点固定；不依赖真实时间流逝或上游实时消息。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn today_history_keeps_fixed_snapshot() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    let day = chrono::Utc::now()
        .with_timezone(&chrono_tz::Asia::Shanghai)
        .format("%Y-%m-%d")
        .to_string();
    let route = format!("/api/communications/sources/{source}/history");
    let body = json!({"start_date":day,"end_date":day});
    let (status, _, first) = h.request("POST", &route, Some(&cookie), body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let job = Uuid::parse_str(first["id"].as_str().unwrap()).unwrap();
    sqlx::query("UPDATE communication_history_jobs SET snapshot_end=snapshot_end-1 WHERE id=$1")
        .bind(job)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let snapshot: i64 =
        sqlx::query_scalar("SELECT snapshot_end FROM communication_history_jobs WHERE id=$1")
            .bind(job)
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    communications::subscription::history_step(&h.state)
        .await
        .unwrap();
    let (_, _, second) = h.request("POST", &route, Some(&cookie), body).await;
    assert_eq!(first["id"], second["id"]);
    communications::subscription::history_step(&h.state)
        .await
        .unwrap();
    let (end, status): (i64, String) =
        sqlx::query_as("SELECT snapshot_end,status FROM communication_history_jobs WHERE id=$1")
            .bind(job)
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert_eq!(end, snapshot);
    assert_eq!(status, "complete");
    // 成功提交的两页才推进计数与时间，不把任务重复入队算作额外进度。
    let (_, _, progress) = h
        .request(
            "GET",
            "/api/communications/history/progress",
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(progress["counts"]["pages_processed"], 2);
    assert_eq!(progress["counts"]["complete"], 1);
    assert!(progress["counts"]["last_progress_at"].is_string());

    server.abort();
    h.close().await;
}

// 验证 800 个来源批量入队、多选去重、失效来源整批拒绝及重试保留游标；不验证真实飞书吞吐或历史可读范围。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn batch_history_selection_and_atomicity() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let ids: Vec<Uuid> = (0..800).map(|_| Uuid::new_v4()).collect();
    let chats: Vec<String> = (0..800).map(|i| format!("oc_batch_{i}")).collect();
    sqlx::query("INSERT INTO communication_sources(id,owner,chat_id,label,start_at,watermark,day_timezone) SELECT id,'admin',chat,chat,0,0,'Asia/Shanghai' FROM UNNEST($1::uuid[],$2::text[]) AS batch(id,chat)")
        .bind(&ids).bind(chats).execute(&h.state.pool).await.unwrap();
    sqlx::query("UPDATE communication_sources SET enabled=false WHERE id=$1")
        .bind(ids[799])
        .execute(&h.state.pool)
        .await
        .unwrap();
    let today = chrono::Utc::now()
        .with_timezone(&chrono_tz::Asia::Shanghai)
        .date_naive();
    let range = json!({"start_date":(today-chrono::Duration::days(364)).to_string(),"end_date":today.to_string()});
    let body =
        json!({"selection":{"scope":"selected","source_ids":[ids[0],ids[1],ids[0]]},"range":range});
    let route = "/api/communications/history";
    assert_eq!(
        h.request("POST", route, None, body.clone()).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, _, result) = h.request("POST", route, Some(&cookie), body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["count"], 2);
    sqlx::query("UPDATE communication_history_jobs SET status='running',page_token='keep_cursor',snapshot_end=snapshot_end-60")
        .execute(&h.state.pool).await.unwrap();
    let before: Vec<(Uuid, String, i64)> = sqlx::query_as(
        "SELECT id,page_token,snapshot_end FROM communication_history_jobs ORDER BY id",
    )
    .fetch_all(&h.state.pool)
    .await
    .unwrap();
    assert_eq!(
        h.request("POST", route, Some(&cookie), body).await.0,
        StatusCode::OK
    );
    let after: Vec<(Uuid, String, i64)> = sqlx::query_as(
        "SELECT id,page_token,snapshot_end FROM communication_history_jobs ORDER BY id",
    )
    .fetch_all(&h.state.pool)
    .await
    .unwrap();
    assert_eq!(before, after);
    // 任一被暂停或已不存在的来源都会使整个多选请求失败，不能留下部分任务。
    for invalid in [ids[799], Uuid::new_v4()] {
        let body =
            json!({"selection":{"scope":"selected","source_ids":[ids[2],invalid]},"range":range});
        assert_eq!(
            h.request("POST", route, Some(&cookie), body).await.0,
            StatusCode::CONFLICT
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM communication_history_jobs")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
    let empty = json!({"selection":{"scope":"selected","source_ids":[]},"range":range});
    assert_eq!(
        h.request("POST", route, Some(&cookie), empty).await.0,
        StatusCode::BAD_REQUEST
    );
    let invalid = json!({"selection":{"scope":"all"},"range":{"start_date":(today-chrono::Duration::days(366)).to_string(),"end_date":today.to_string()}});
    assert_eq!(
        h.request("POST", route, Some(&cookie), invalid).await.0,
        StatusCode::BAD_REQUEST
    );
    let all = json!({"selection":{"scope":"all"},"range":range});
    let (status, _, result) = h.request("POST", route, Some(&cookie), all.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["count"], 799);
    assert_eq!(
        h.request("POST", route, Some(&cookie), all).await.0,
        StatusCode::OK
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM communication_history_jobs")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 799);
    let paused: i64 =
        sqlx::query_scalar("SELECT count(*) FROM communication_history_jobs WHERE source_id=$1")
            .bind(ids[799])
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert_eq!(paused, 0);
    // 全量进度不受 100 条展示上限影响，暂停状态独立计数，内部游标不可出现在响应中。
    sqlx::query("UPDATE communication_sources SET enabled=false WHERE id=$1")
        .bind(ids[0])
        .execute(&h.state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE communication_history_jobs SET status='complete' WHERE source_id=$1")
        .bind(ids[2])
        .execute(&h.state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE communication_history_jobs SET status='failed',error='communication_unavailable',pages_processed=3,last_progress_at=now() WHERE source_id=$1").bind(ids[3]).execute(&h.state.pool).await.unwrap();
    sqlx::query("UPDATE communication_history_jobs SET pages_processed=12,last_progress_at=now() WHERE source_id=$1").bind(ids[1]).execute(&h.state.pool).await.unwrap();
    let route = "/api/communications/history/progress";
    assert_eq!(
        h.request("GET", route, None, Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, _, progress) = h.request("GET", route, Some(&cookie), Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(progress["counts"]["total"], 799);
    assert_eq!(progress["counts"]["pending"], 795);
    for state in ["running", "complete", "failed", "paused"] {
        assert_eq!(progress["counts"][state], 1);
    }
    assert_eq!(progress["counts"]["pages_processed"], 15);
    assert!(progress["counts"]["last_progress_at"].is_string());
    assert_eq!(progress["jobs"].as_array().unwrap().len(), 20);
    assert_eq!(progress["jobs"][0]["status"], "failed");
    assert!(!progress.to_string().contains("keep_cursor"));
    server.abort();
    h.close().await;
}

// 验证取消围栏能拒绝在途响应，即刻重提不会复活旧页；不验证真实上游中止 HTTP 的能力。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn cancellation_fences_inflight_and_resubmission() {
    let (h, fixture, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    let day = chrono::DateTime::from_timestamp_millis(fixture.lock().unwrap().base)
        .unwrap()
        .with_timezone(&chrono_tz::Asia::Shanghai)
        .date_naive()
        .to_string();
    let route = format!("/api/communications/sources/{source}/history");
    let body = json!({"start_date":day,"end_date":day});
    let (_, _, first) = h.request("POST", &route, Some(&cookie), body.clone()).await;
    fixture.lock().unwrap().slow = true;
    let state = h.state.clone();
    let inflight = tokio::spawn(async move {
        communications::subscription::history_step(&state)
            .await
            .unwrap();
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if fixture.lock().unwrap().reads > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let cancel = json!({"scope":"job","id":first["id"]});
    assert_eq!(
        h.request(
            "POST",
            "/api/communications/history/cancel",
            None,
            cancel.clone()
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, _, cancelled) = h
        .request(
            "POST",
            "/api/communications/history/cancel",
            Some(&cookie),
            cancel,
        )
        .await;
    assert_eq!(cancelled["count"], 1);
    let (_, _, resumed) = h.request("POST", &route, Some(&cookie), body).await;
    assert_eq!(resumed["id"], first["id"]);
    inflight.await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM communication_documents")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "取消前的响应不能写入重提任务");
    let status: String = sqlx::query_scalar("SELECT status FROM communication_history_jobs")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(status, "pending");
    fixture.lock().unwrap().slow = false;
    communications::subscription::history_step(&h.state)
        .await
        .unwrap();
    let (_, _, cancelled) = h
        .request(
            "POST",
            "/api/communications/history/cancel",
            Some(&cookie),
            json!({"scope":"all"}),
        )
        .await;
    assert_eq!(cancelled["count"], 1);
    let reads = fixture.lock().unwrap().reads;
    communications::subscription::history_step(&h.state)
        .await
        .unwrap();
    assert_eq!(fixture.lock().unwrap().reads, reads);
    let (_, _, progress) = h
        .request(
            "GET",
            "/api/communications/history/progress",
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(progress["counts"]["cancelled"], 1);
    assert_eq!(progress["counts"]["running"], 0);
    let (docs,enabled):(i64,bool)=sqlx::query_as("SELECT (SELECT count(*) FROM communication_documents),enabled FROM communication_sources WHERE id=$1").bind(source).fetch_one(&h.state.pool).await.unwrap();
    assert_eq!(docs, 1, "取消保留已保存原文");
    assert!(enabled, "新消息订阅继续启用");
    server.abort();
    h.close().await;
}

// 同一天 60 条原文跨飞书分页全部保存、前端按 50 条分页、成员第二页姓名生效；使用本地协议夹具。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn daily_messages_are_not_capped_at_five() {
    let (h, fixture, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    add(&h, &cookie).await;
    fixture.lock().unwrap().bulk = true;
    for _ in 0..2 {
        due(&h).await;
        communications::sync::step(&h.state).await.unwrap();
    }
    let id: Uuid = sqlx::query_scalar("SELECT id FROM communication_documents")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    let (_, _, first) = h
        .request(
            "GET",
            &format!("/api/communications/documents/{id}"),
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(first["total"], 60);
    assert_eq!(first["messages"].as_array().unwrap().len(), 50);
    assert_eq!(first["messages"][0]["sender_name"], "小林");
    let (_, _, last) = h
        .request(
            "GET",
            &format!("/api/communications/documents/{id}?offset=50"),
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(last["messages"].as_array().unwrap().len(), 10);
    // 应用身份不能拿真人成员名称替代，也不从通知内容猜测机器人名称。
    assert_eq!(
        last["messages"][9]["sender_name"],
        "应用机器人（名称未获取）"
    );
    server.abort();
    h.close().await;
}

/// 验证日期探测的边界、认证和无订阅副作用；仅使用模拟消息，不验证租户历史完整性。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn activity_range_is_read_only_and_validated() {
    let (h, fixture, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let day = chrono::DateTime::from_timestamp_millis(fixture.lock().unwrap().base)
        .unwrap()
        .with_timezone(&chrono_tz::Asia::Shanghai)
        .date_naive();
    for (date, expected) in [(day, true), (day.pred_opt().unwrap(), false)] {
        let input = json!({"chat_id":"oc_fixture","range":{"start_date":date.to_string(),"end_date":date.to_string()}});
        let (status, _, result) = h
            .request(
                "POST",
                "/api/communications/chats/activity",
                Some(&cookie),
                input,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(result["active"], expected);
    }
    let input = json!({"chat_id":"oc_fixture","range":{"start_date":"bad","end_date":"bad"}});
    assert_eq!(
        h.request(
            "POST",
            "/api/communications/chats/activity",
            Some(&cookie),
            input.clone()
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        h.request("POST", "/api/communications/chats/activity", None, input)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM communication_sources")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    server.abort();
    h.close().await;
}
