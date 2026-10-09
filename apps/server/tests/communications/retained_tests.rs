use super::*;

/// 构造确有排除记录的历史来源，覆盖不能依赖自动恢复的场景。
async fn retained_source(h: &Harness, chat: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO communication_sources(id,owner,chat_id,label,start_at,watermark,subscribed,enabled,version,day_timezone) VALUES($1,'admin',$2,$2,1,2,false,false,2,'Asia/Shanghai')")
        .bind(id).bind(chat).execute(&h.state.pool).await.unwrap();
    sqlx::query("INSERT INTO communication_exclusions(owner,chat_id) VALUES('admin',$1)")
        .bind(chat)
        .execute(&h.state.pool)
        .await
        .unwrap();
    id
}

// 验证显式批量恢复解除历史排除且保留文件、取消任务与未选项；真实数据库加本地夹具，不代表生产补采完成。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn retained_restore_is_atomic_and_preserves_documents() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    let doc = import(&h, &cookie).await;
    assert_eq!(
        h.request(
            "POST",
            "/api/communications/sources/remove",
            Some(&cookie),
            json!({"selection":{"scope":"one","id":source,"version":1}})
        )
        .await
        .0,
        StatusCode::OK
    );
    let second = retained_source(&h, "oc_second").await;
    let untouched = retained_source(&h, "oc_untouched").await;
    sqlx::query("INSERT INTO communication_history_jobs(id,source_id,start_at,end_at,snapshot_end,status) VALUES($1,$2,1,2,2,'cancelled')")
        .bind(Uuid::new_v4()).bind(source).execute(&h.state.pool).await.unwrap();
    let sources = json!([{"id":source,"version":2},{"id":second,"version":2}]);
    let (status, _, result) = h
        .request(
            "POST",
            "/api/communications/sources/restore",
            Some(&cookie),
            json!({"sources":sources}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["restored"], 2);
    for id in [source, second] {
        let row: (bool, bool, i64, bool, bool) = sqlx::query_as("SELECT subscribed,enabled,version,start_at=watermark AND start_at>=extract(epoch FROM now())::bigint-10,page_token='' AND window_start IS NULL AND window_end IS NULL AND error IS NULL AND next_sync<=now() FROM communication_sources WHERE id=$1")
            .bind(id).fetch_one(&h.state.pool).await.unwrap();
        assert_eq!(row, (true, true, 3, true, true));
    }
    assert!(
        !sqlx::query_scalar::<_, bool>("SELECT subscribed FROM communication_sources WHERE id=$1")
            .bind(untouched)
            .fetch_one(&h.state.pool)
            .await
            .unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT chat_id FROM communication_exclusions")
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        "oc_untouched"
    );
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
    assert_eq!(
        h.request(
            "GET",
            &format!(
                "/api/communications/documents/{}",
                doc["id"].as_str().unwrap()
            ),
            Some(&cookie),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    // 重复提交旧确认不能重置采集起点或再增加版本。
    assert_eq!(
        h.request(
            "POST",
            "/api/communications/sources/restore",
            Some(&cookie),
            json!({"sources":sources})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    server.abort();
    h.close().await;
}

// 验证批量删除的版本原子核对、未选项隔离和磁盘失败后重试；夹具文件故障不代表真实磁盘故障验收。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn retained_delete_reports_partial_failure_and_keeps_exclusions() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let good = add(&h, &cookie).await;
    import(&h, &cookie).await;
    h.request(
        "POST",
        "/api/communications/sources/remove",
        Some(&cookie),
        json!({"selection":{"scope":"one","id":good,"version":1}}),
    )
    .await;
    let broken = retained_source(&h, "oc_broken").await;
    let untouched = retained_source(&h, "oc_untouched").await;
    let directory = h
        .state
        .config
        .memory
        .as_ref()
        .unwrap()
        .directory
        .join("_communications")
        .join(broken.to_string());
    std::fs::write(&directory, "夹具：应为目录的位置是文件").unwrap();
    // 任一版本失效时，连版本正确的来源也不能先被删除。
    assert_eq!(h.request("POST", "/api/communications/sources/remove", Some(&cookie), json!({"selection":{"scope":"retained","sources":[{"id":good,"version":2},{"id":broken,"version":1}]},"delete_documents":true})).await.0, StatusCode::CONFLICT);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM communication_sources")
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        3
    );
    let (status, _, result) = h.request("POST", "/api/communications/sources/remove", Some(&cookie), json!({"selection":{"scope":"retained","sources":[{"id":good,"version":2},{"id":broken,"version":2}]},"delete_documents":true})).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let job = result["job_id"].as_str().unwrap().to_owned();
    let progress = finish_removal(&h, &cookie).await;
    assert_eq!(progress["complete"], 1);
    assert_eq!(progress["failed"], 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM communication_documents WHERE source_id=$1"
        )
        .bind(good)
        .fetch_one(&h.state.pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT version FROM communication_sources WHERE id=$1")
            .bind(untouched)
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM communication_exclusions")
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        3
    );
    std::fs::remove_file(&directory).unwrap();
    assert_eq!(
        h.request(
            "POST",
            &format!("/api/communications/removals/{job}/retry"),
            Some(&cookie),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    let progress = finish_removal(&h, &cookie).await;
    assert_eq!(progress["complete"], 2);
    assert_eq!(progress["failed"], 0);
    server.abort();
    h.close().await;
}

// 验证批量入口的认证、数量、重复、状态与版本边界；不覆盖真实飞书授权有效性。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn retained_batches_reject_invalid_or_stale_selection() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let id = retained_source(&h, "oc_retained").await;
    let active = add(&h, &cookie).await;
    let (_, _, link) = h
        .request(
            "POST",
            "/api/admin/links",
            Some(&cookie),
            json!({"label":"批量历史操作访客夹具","expires_in_seconds":3600}),
        )
        .await;
    let token = link["url"]
        .as_str()
        .unwrap()
        .split("token=")
        .nth(1)
        .unwrap();
    let (_, guest, _) = h
        .request("POST", "/api/auth/exchange", None, json!({"token":token}))
        .await;
    let valid = json!([{"id":id,"version":2}]);
    let mut oversized = Vec::new();
    for _ in 0..1001 {
        oversized.push(json!({"id":Uuid::new_v4(),"version":2}));
    }
    for restore in [true, false] {
        let path = if restore {
            "/api/communications/sources/restore"
        } else {
            "/api/communications/sources/remove"
        };
        let body = |sources: Value| {
            if restore {
                json!({"sources":sources})
            } else {
                json!({"selection":{"scope":"retained","sources":sources},"delete_documents":true})
            }
        };
        assert_eq!(
            h.request("POST", path, None, body(valid.clone())).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            h.request("POST", path, guest.as_deref(), body(valid.clone()))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        for sources in [
            json!([]),
            json!([{"id":id,"version":2},{"id":id,"version":2}]),
            json!(oversized),
        ] {
            assert_eq!(
                h.request("POST", path, Some(&cookie), body(sources))
                    .await
                    .0,
                StatusCode::BAD_REQUEST
            );
        }
        for sources in [
            json!([{"id":id,"version":1}]),
            json!([{"id":Uuid::new_v4(),"version":2}]),
            json!([{"id":id,"version":2},{"id":active,"version":1}]),
        ] {
            assert_eq!(
                h.request("POST", path, Some(&cookie), body(sources))
                    .await
                    .0,
                StatusCode::CONFLICT
            );
        }
    }
    assert_eq!(
        h.request(
            "POST",
            "/api/communications/sources/remove",
            Some(&cookie),
            json!({"selection":{"scope":"retained","sources":valid}})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT version FROM communication_sources WHERE id=$1")
            .bind(id)
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        2
    );
    server.abort();
    h.close().await;
}
