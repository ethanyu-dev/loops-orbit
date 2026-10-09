use super::*;

/// 从同一状态响应取得弹窗版本摘要，测试不自行伪造确认范围。
async fn all_selection(h: &Harness, cookie: &str) -> Value {
    let (_, _, snapshot) = h
        .request(
            "GET",
            "/api/communications/status",
            Some(cookie),
            Value::Null,
        )
        .await;
    json!({"scope":"all","revision":snapshot["subscription_revision"]})
}

// 使用真实 PostgreSQL 与本地文件验证移除保留、旧确认拒绝、任务围栏及重新订阅；不修改真实飞书消息或调用真实模型。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn removal_preserves_documents_and_requires_a_current_snapshot() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    let doc = import(&h, &cookie).await;
    let selection = all_selection(&h, &cookie).await;
    sqlx::query("UPDATE communication_sources SET version=version+1 WHERE id=$1")
        .bind(source)
        .execute(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(
        h.request(
            "POST",
            "/api/communications/sources/remove",
            Some(&cookie),
            json!({"selection":selection})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT subscribed FROM communication_sources WHERE id=$1")
            .bind(source)
            .fetch_one(&h.state.pool)
            .await
            .unwrap()
    );
    let version: i64 = sqlx::query_scalar("SELECT version FROM communication_sources WHERE id=$1")
        .bind(source)
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO communication_history_jobs(id,source_id,start_at,end_at,snapshot_end,status) VALUES($1,$2,1,2,2,'running')").bind(Uuid::new_v4()).bind(source).execute(&h.state.pool).await.unwrap();
    let (status, _, result) = h
        .request(
            "POST",
            "/api/communications/sources/remove",
            Some(&cookie),
            json!({"selection":all_selection(&h,&cookie).await}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["removed"], 1);
    let (subscribed, enabled, new_version): (bool, bool, i64) =
        sqlx::query_as("SELECT subscribed,enabled,version FROM communication_sources WHERE id=$1")
            .bind(source)
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert!(!subscribed && !enabled && new_version > version);
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
    let (status, _, detail) = h
        .request(
            "GET",
            &format!(
                "/api/communications/documents/{}",
                doc["id"].as_str().unwrap()
            ),
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(detail["total"].as_u64().unwrap() > 0);
    assert_eq!(
        h.request(
            "PUT",
            &format!("/api/communications/sources/{source}"),
            Some(&cookie),
            json!({"version":new_version,"enabled":true})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (_, _, files) = h
        .request(
            "GET",
            "/api/communications/library/files",
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(files["items"][0]["source_subscribed"], false);
    // 显式重新选择会话复用原文与来源 ID，不恢复已经取消的历史任务。
    assert_eq!(add(&h, &cookie).await, source);
    let (subscribed, enabled): (bool, bool) =
        sqlx::query_as("SELECT subscribed,enabled FROM communication_sources WHERE id=$1")
            .bind(source)
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert!(subscribed && enabled);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM communication_documents WHERE source_id=$1"
        )
        .bind(source)
        .fetch_one(&h.state.pool)
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM communication_exclusions")
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        0
    );
    server.abort();
    h.close().await;
}

// 验证批量删除成功项和磁盘故障项分开报告，失败来源停用且可重试；不验证系统磁盘损坏或真实飞书权限。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn removal_reports_partial_deletion_and_can_retry() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    import(&h, &cookie).await;
    let broken = Uuid::new_v4();
    sqlx::query("INSERT INTO communication_sources(id,owner,chat_id,label,start_at,watermark) VALUES($1,'admin','oc_broken','清理失败夹具',0,0)").bind(broken).execute(&h.state.pool).await.unwrap();
    let directory = h
        .state
        .config
        .memory
        .as_ref()
        .unwrap()
        .directory
        .join("_communications")
        .join(broken.to_string());
    std::fs::write(&directory, "夹具：目录位置被普通文件占据").unwrap();
    let (status, _, result) = h
        .request(
            "POST",
            "/api/communications/sources/remove",
            Some(&cookie),
            json!({"selection":all_selection(&h,&cookie).await,"delete_documents":true}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let job = result["job_id"].as_str().unwrap().to_owned();
    let progress = finish_removal(&h, &cookie).await;
    assert_eq!(progress["complete"], 1);
    assert_eq!(progress["failed"], 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM communication_documents WHERE source_id=$1"
        )
        .bind(source)
        .fetch_one(&h.state.pool)
        .await
        .unwrap(),
        0
    );
    assert!(
        !h.state
            .config
            .memory
            .as_ref()
            .unwrap()
            .directory
            .join("_communications")
            .join(source.to_string())
            .exists()
    );
    let (subscribed, enabled): (bool, bool) =
        sqlx::query_as("SELECT subscribed,enabled FROM communication_sources WHERE id=$1")
            .bind(broken)
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert!(!subscribed && !enabled);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM communication_connections")
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        1
    );
    std::fs::remove_file(directory).unwrap();
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

// 验证批量移除仅限管理员且拒绝缺失范围的请求；不覆盖外部身份服务。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn removal_rejects_guests_and_invalid_input() {
    let h = Harness::new().await;
    let cookie = h.login().await;
    let (_, _, link) = h
        .request(
            "POST",
            "/api/admin/links",
            Some(&cookie),
            json!({"label":"移除测试访客","expires_in_seconds":3600}),
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
    let body = json!({"selection":{"scope":"all","revision":"stale"}});
    assert_eq!(
        h.request(
            "POST",
            "/api/communications/sources/remove",
            None,
            body.clone()
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        h.request(
            "POST",
            "/api/communications/sources/remove",
            guest.as_deref(),
            body
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        h.request(
            "POST",
            "/api/communications/sources/remove",
            Some(&cookie),
            json!({})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    h.close().await;
}
