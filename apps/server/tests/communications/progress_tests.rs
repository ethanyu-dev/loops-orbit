use super::*;

// 真实数据库验证超百份资料时状态读取不拿采集锁、不依赖文件；不模拟生产容量或上游网络延迟。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn status_reads_metadata_without_waiting_for_collection_lock() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    sqlx::query("INSERT INTO communication_documents(id,source_id,day,raw_hash) SELECT gen_random_uuid(),$1,'fixture-'||n,repeat('a',64) FROM generate_series(1,105) n")
        .bind(source).execute(&h.state.pool).await.unwrap();
    let guard = h.state.communications.lock().await;
    let (status, _, value) = tokio::time::timeout(
        Duration::from_secs(2),
        h.request(
            "GET",
            "/api/communications/status",
            Some(&cookie),
            Value::Null,
        ),
    )
    .await
    .expect("状态读取不能等待采集锁");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["documents"].as_array().unwrap().len(), 100);
    assert_eq!(value["progress"][0]["total"], 105);
    assert_eq!(value["progress"][0]["checking"], 105);
    assert_eq!(value["progress"][0]["ready"], 0);
    drop(guard);
    // 首轮只处理十份缺失文件，其余仍显示未知，不能把尚未检查的资料算作成功。
    communications::progress::step(&h.state).await.unwrap();
    let (_, _, value) = h
        .request(
            "GET",
            "/api/communications/status",
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(value["progress"][0]["errors"], 10);
    assert_eq!(value["progress"][0]["checking"], 95);
    assert_eq!(
        h.request("GET", "/api/communications/status", None, Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    server.abort();
    h.close().await;
}

// 原文、摘要、pgvector 由本地协议夹具生成；验证快照失效、文件损坏及级联清理，不代表真实模型语义验收。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn document_progress_rechecks_versions_files_and_index_configuration() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    let doc = import(&h, &cookie).await;
    let id = Uuid::parse_str(doc["id"].as_str().unwrap()).unwrap();
    communications::progress::step(&h.state).await.unwrap();
    let (_, _, value) = h
        .request(
            "GET",
            "/api/communications/status",
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(value["progress"][0]["ready"], 1);
    sqlx::query("UPDATE communication_document_progress SET embedding_version='old-model'")
        .execute(&h.state.pool)
        .await
        .unwrap();
    let (_, _, value) = h
        .request(
            "GET",
            "/api/communications/status",
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(value["progress"][0]["ready"], 0);
    assert_eq!(value["progress"][0]["checking"], 1);
    communications::progress::step(&h.state).await.unwrap();
    // 摘要变化即刻排除旧快照，而不等待定时扫描。
    sqlx::query(
        "UPDATE communication_documents SET version=version+1,summary_hash=NULL WHERE id=$1",
    )
    .bind(id)
    .execute(&h.state.pool)
    .await
    .unwrap();
    let (_, _, value) = h
        .request(
            "GET",
            "/api/communications/status",
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(value["progress"][0]["checking"], 1);
    communications::progress::step(&h.state).await.unwrap();
    let (_, _, value) = h
        .request(
            "GET",
            "/api/communications/status",
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(value["progress"][0]["summarizing"], 1);
    let file = h
        .state
        .config
        .memory
        .as_ref()
        .unwrap()
        .directory
        .join("_communications")
        .join(source.to_string())
        .join(id.to_string())
        .join(format!("{}.jsonl", doc["raw_hash"].as_str().unwrap()));
    std::fs::write(file, "损坏的原文").unwrap();
    sqlx::query("UPDATE communication_document_progress SET checked_at=now()-interval '2 minutes'")
        .execute(&h.state.pool)
        .await
        .unwrap();
    communications::progress::step(&h.state).await.unwrap();
    let (_, _, value) = h
        .request(
            "GET",
            "/api/communications/status",
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(value["progress"][0]["errors"], 1);
    sqlx::query("DELETE FROM communication_sources WHERE id=$1")
        .bind(source)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM communication_document_progress")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    server.abort();
    h.close().await;
}
