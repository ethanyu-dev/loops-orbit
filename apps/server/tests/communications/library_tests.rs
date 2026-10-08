use super::*;

// 验证完整历史目录、同日多群、文件分页、名称和日期搜索以及暂停来源展示；只读元数据，不验收真实飞书或模型摘要。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn library_browses_full_history_and_filters_files() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    sqlx::query("UPDATE communication_sources SET label='Launch 100%_组' WHERE id=$1")
        .bind(source)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let other = Uuid::new_v4();
    sqlx::query("INSERT INTO communication_sources(id,owner,chat_id,label,start_at,watermark,enabled) VALUES($1,'admin','oc_other','另一讨论组',0,0,false)")
        .bind(other).execute(&h.state.pool).await.unwrap();
    for id in [source, other] {
        sqlx::query("INSERT INTO communication_documents(id,source_id,day,raw_hash) SELECT gen_random_uuid(),$1,('2026-10-08'::date-n)::text,repeat('a',64) FROM generate_series(0,69) n")
            .bind(id).execute(&h.state.pool).await.unwrap();
    }
    // 目录读取不能等候采集任务释放锁，也不能依赖尚不存在的原文文件。
    let guard = h.state.communications.lock().await;
    let (status, _, days) = tokio::time::timeout(
        Duration::from_secs(2),
        h.request(
            "GET",
            "/api/communications/library/days",
            Some(&cookie),
            Value::Null,
        ),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(days["items"].as_array().unwrap().len(), 60);
    assert_eq!(days["items"][0]["day"], "2026-10-08");
    assert_eq!(days["items"][0]["count"], 2);
    let cursor = days["next_before"].as_str().unwrap();
    let (_, _, older) = h
        .request(
            "GET",
            &format!("/api/communications/library/days?before={cursor}"),
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(older["items"].as_array().unwrap().len(), 10);
    assert!(older["next_before"].is_null());
    assert!(older["items"][0]["day"].as_str().unwrap() < cursor);
    for offset in [0, 50, 100] {
        let (status, _, files) = h
            .request(
                "GET",
                &format!("/api/communications/library/files?offset={offset}"),
                Some(&cookie),
                Value::Null,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(files["total"], 140);
        assert_eq!(
            files["items"].as_array().unwrap().len(),
            if offset == 100 { 40 } else { 50 }
        );
        assert_eq!(
            files["next_offset"],
            if offset == 100 {
                Value::Null
            } else {
                json!(offset + 50)
            }
        );
        assert!(files["items"][0].get("raw_hash").is_none());
    }
    let (_, _, day) = h
        .request(
            "GET",
            "/api/communications/library/files?day=2026-10-08",
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(day["total"], 2);
    assert!(
        day["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["source_enabled"] == false)
    );
    for query in ["launch", "100%25_", "2026-08"] {
        let (status, _, files) = h
            .request(
                "GET",
                &format!("/api/communications/library/files?q={query}"),
                Some(&cookie),
                Value::Null,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(files["total"], if query == "2026-08" { 62 } else { 70 });
    }
    let (_, _, empty) = h
        .request(
            "GET",
            "/api/communications/library/files?q=missing",
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(empty["total"], 0);
    assert!(empty["items"].as_array().unwrap().is_empty());
    drop(guard);
    server.abort();
    h.close().await;
}

// 验证访客及未登录者无法枚举目录、非法日期和分页会被拒绝；不覆盖生产网络和身份提供方。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn library_rejects_unauthorized_and_invalid_queries() {
    let h = Harness::new().await;
    let cookie = h.login().await;
    let (_, _, link) = h
        .request(
            "POST",
            "/api/admin/links",
            Some(&cookie),
            json!({"label":"目录测试访客","expires_in_seconds":3600}),
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
    for path in [
        "/api/communications/library/days",
        "/api/communications/library/files",
    ] {
        assert_eq!(
            h.request("GET", path, None, Value::Null).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            h.request("GET", path, guest.as_deref(), Value::Null)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    for path in [
        "/api/communications/library/days?before=2026-02-30",
        "/api/communications/library/files?day=2026-2-01",
        "/api/communications/library/files?offset=-1",
    ] {
        assert_eq!(
            h.request("GET", path, Some(&cookie), Value::Null).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let long_query = format!("/api/communications/library/files?q={}", "a".repeat(2001));
    assert_eq!(
        h.request("GET", &long_query, Some(&cookie), Value::Null)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    h.close().await;
}
