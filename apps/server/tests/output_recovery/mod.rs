use super::*;

// 真实 PostgreSQL 与本地 HTTP 夹具验证恢复、失败持久化和飞书出站文案；不调用真实模型或飞书。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn persists_recovered_answer_and_specific_terminal_failures() {
    let h = Harness::new().await;
    let admin = h.login().await;
    let conversation = h.conversation(&admin).await;
    h.send(&admin, conversation, "recover-output-limit", Uuid::new_v4())
        .await;
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(worker::run_worker(h.state.clone(), receiver));
    wait_for(&h.state, "completed", 1).await;
    let content: String = sqlx::query_scalar("SELECT content FROM messages WHERE role='assistant'")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(content, "恢复后的完整答案");
    assert_eq!(h.requests.lock().unwrap().len(), 2);
    stop.send(true).unwrap();
    worker.await.unwrap();

    for (index, (input, code, expected_text)) in [
        (
            "always-output-limit",
            "provider_output_limit",
            "模型输出达到上限",
        ),
        (
            "filtered-output",
            "provider_content_filtered",
            "模型服务过滤了本次输出",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let run = h.send(&admin, conversation, input, Uuid::new_v4()).await;
        // 仅标记需要飞书回复，不启动真实发送器，随后直接检查出站队列。
        sqlx::query("UPDATE runs SET reply_to='fixture-reply' WHERE id=$1")
            .bind(run)
            .execute(&h.state.pool)
            .await
            .unwrap();
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let worker = tokio::spawn(worker::run_worker(h.state.clone(), receiver));
        wait_for(&h.state, "failed", index as i64 + 1).await;
        stop.send(true).unwrap();
        worker.await.unwrap();
        let record: (String, i32, String) =
            sqlx::query_as("SELECT error,attempts,partial_content FROM runs WHERE id=$1")
                .bind(run)
                .fetch_one(&h.state.pool)
                .await
                .unwrap();
        assert_eq!(record, (code.to_owned(), 1, String::new()));
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM messages WHERE run_id=$1 AND role='assistant'",
        )
        .bind(run)
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
        assert_eq!(count, 0);
        let reply: String = sqlx::query_scalar("SELECT content FROM outbox WHERE id=$1")
            .bind(run)
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
        assert!(reply.contains(expected_text));
    }
    assert_eq!(h.requests.lock().unwrap().len(), 5);
    h.close().await;
}
