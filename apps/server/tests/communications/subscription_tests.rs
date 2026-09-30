use super::*;

// 验证自动发现只从开启时间订阅、不回填历史、重复发现幂等和移除排除；上游是本地协议夹具。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn discovery_since_and_exclusion() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    communications::subscription::discover(&h.state)
        .await
        .unwrap();
    let (id, start, watermark): (Uuid, i64, i64) =
        sqlx::query_as("SELECT id,start_at,watermark FROM communication_sources")
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    let since: i64 = sqlx::query_scalar("SELECT subscription_since FROM communication_connections")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(start, since);
    assert_eq!(watermark, since);
    for _ in 0..2 {
        due(&h).await;
        communications::sync::step(&h.state).await.unwrap();
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM communication_documents")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "过去的夹具消息必须被过滤");
    sqlx::query("UPDATE communication_connections SET next_discovery=now()")
        .execute(&h.state.pool)
        .await
        .unwrap();
    communications::subscription::discover(&h.state)
        .await
        .unwrap();
    assert_eq!(
        h.request(
            "DELETE",
            &format!("/api/communications/sources/{id}"),
            Some(&cookie),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    sqlx::query("UPDATE communication_connections SET next_discovery=now()")
        .execute(&h.state.pool)
        .await
        .unwrap();
    communications::subscription::discover(&h.state)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM communication_sources")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        h.request(
            "PUT",
            "/api/communications/settings",
            None,
            json!({"auto_subscribe":false})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        h.request(
            "PUT",
            "/api/communications/settings",
            Some(&cookie),
            json!({"auto_subscribe":false})
        )
        .await
        .0,
        StatusCode::OK
    );
    server.abort();
    h.close().await;
}

// 验证手选历史独立于增量水位、重复补录不增版本、错误日期被拒绝；不验证飞书实际历史覆盖范围。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn history_is_independent_and_idempotent() {
    let (h, fixture, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    communications::subscription::discover(&h.state)
        .await
        .unwrap();
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
