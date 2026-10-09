use super::*;

// 验证 704 项删除在实际清理前返回受理，重建应用状态后仍能续跑；本地数据库时限不代表生产吞吐基准。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn removal_job_accepts_large_batch_before_io_and_survives_restart() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let rows: Vec<(Uuid,i64)> = sqlx::query_as("INSERT INTO communication_sources(id,owner,chat_id,label,start_at,watermark,subscribed,enabled,day_timezone) SELECT gen_random_uuid(),'admin','oc_bulk_'||n,'大批量夹具 '||n,0,0,false,false,'Asia/Shanghai' FROM generate_series(1,704) n RETURNING id,version")
        .fetch_all(&h.state.pool).await.unwrap();
    let sources: Vec<Value> = rows
        .iter()
        .map(|(id, version)| json!({"id":id,"version":version}))
        .collect();
    let body = json!({"selection":{"scope":"retained","sources":sources},"delete_documents":true});
    let (status, _, result) = tokio::time::timeout(
        Duration::from_secs(5),
        h.request(
            "POST",
            "/api/communications/sources/remove",
            Some(&cookie),
            body.clone(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(result["queued"], 704);
    assert_eq!(result["removed"], 0);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM communication_sources WHERE removal_pending AND NOT subscribed AND NOT enabled").fetch_one(&h.state.pool).await.unwrap();
    assert_eq!(count, 704);
    // 已受理任务不因旧请求重发而重复入队，也不允许单项恢复抢走待删来源。
    assert_eq!(
        h.request(
            "POST",
            "/api/communications/sources/remove",
            Some(&cookie),
            body
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        h.request(
            "POST",
            "/api/communications/sources/restore",
            Some(&cookie),
            json!({"sources":[{"id":rows[0].0,"version":2}]})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (_, _, snapshot) = h
        .request(
            "GET",
            "/api/communications/status",
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(snapshot["removals"][0]["pending"], 704);
    assert_eq!(snapshot["removals"][0]["complete"], 0);
    // 新 AppState 没有原请求的内存状态，仍从数据库取回任务。
    let restarted = AppState::new((*h.state.config).clone(), h.state.pool.clone()).unwrap();
    assert!(
        communications::removal_jobs::step(&restarted)
            .await
            .unwrap()
    );
    assert!(
        communications::removal_jobs::step(&restarted)
            .await
            .unwrap()
    );
    let (_, _, snapshot) = h
        .request(
            "GET",
            "/api/communications/status",
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(snapshot["removals"][0]["complete"], 1);
    assert_eq!(snapshot["removals"][0]["pending"], 703);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM communication_removal_batches")
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        1
    );
    server.abort();
    h.close().await;
}

// 验证文件已删但任务未提交的恢复、版本围栏及跨实例领取锁；不模拟操作系统崩溃或真实网络超时。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn removal_job_replay_and_version_fence() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let first = add(&h, &cookie).await;
    let second = Uuid::new_v4();
    sqlx::query("INSERT INTO communication_sources(id,owner,chat_id,label,start_at,watermark) VALUES($1,'admin','oc_second','围栏夹具',0,0)").bind(second).execute(&h.state.pool).await.unwrap();
    let (_, _, snapshot) = h
        .request(
            "GET",
            "/api/communications/status",
            Some(&cookie),
            Value::Null,
        )
        .await;
    let (status, _, queued) = h.request("POST", "/api/communications/sources/remove", Some(&cookie), json!({"selection":{"scope":"all","revision":snapshot["subscription_revision"]},"delete_documents":true})).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let job = Uuid::parse_str(queued["job_id"].as_str().unwrap()).unwrap();
    assert_eq!(
        h.request(
            "POST",
            "/api/communications/sources",
            Some(&cookie),
            json!({"chat_id":"oc_fixture","label":"不能恢复待删除来源"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        h.request(
            "DELETE",
            &format!("/api/communications/sources/{first}"),
            Some(&cookie),
            Value::Null
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let mut lease = h.state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM communication_removal_batches WHERE id=$1 FOR UPDATE")
        .bind(job)
        .execute(&mut *lease)
        .await
        .unwrap();
    assert!(!communications::removal_jobs::step(&h.state).await.unwrap());
    lease.rollback().await.unwrap();
    // 模拟来源删除成功但进度事务尚未提交，以及另一个来源发生意外版本变动。
    sqlx::query("DELETE FROM communication_sources WHERE id=$1")
        .bind(first)
        .execute(&h.state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE communication_sources SET version=version+1 WHERE id=$1")
        .bind(second)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let result = finish_removal(&h, &cookie).await;
    assert_eq!(result["complete"], 1);
    assert_eq!(result["failed"], 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM communication_sources WHERE id=$1")
            .bind(second)
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        h.request(
            "POST",
            &format!("/api/communications/removals/{job}/retry"),
            None,
            Value::Null
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    server.abort();
    h.close().await;
}
