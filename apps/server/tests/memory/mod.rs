use super::*;
use orbit_server::memory::{
    self as memories, Entry,
    config::{EmbeddingConfig, MemoryConfig},
};

/// 在隔离 schema 上开启真实文件与索引，供应商调用仍是本地 HTTP 夹具。
pub(super) async fn setup(semantic: bool) -> Harness {
    let mut h = Harness::new().await;
    let mut config = (*h.state.config).clone();
    config.memory = Some(MemoryConfig {
        directory: std::env::temp_dir().join(format!("orbit-memory-{}", Uuid::new_v4())),
        auto_extract: true,
        embedding: semantic.then(|| EmbeddingConfig {
            base_url: config.model.base_url.clone(),
            api_key: "fixture".into(),
            model: "fixture-embedding".into(),
            dimensions: 3,
            revision: "1".into(),
            min_similarity: 0.8,
        }),
    });
    h.state = AppState::new(config, h.state.pool.clone()).unwrap();
    memories::initialize(&h.state).await.unwrap();
    h.app = router(h.state.clone());
    h
}
/// 测试原文始终由服务端 API 写入，保留与真实使用相同的认证和校验。
pub(super) async fn create(
    h: &Harness,
    cookie: &str,
    key: &str,
    content: &str,
    kind: &str,
) -> Entry {
    let (status, _, entry) = h
        .request(
            "POST",
            "/api/memories",
            Some(cookie),
            json!({"key":key,"content":content,"kind":kind}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{entry}");
    serde_json::from_value(entry).unwrap()
}
/// 等待正常 worker 完成真实持久化流程，返回后台任务来源 ID。
pub(super) async fn complete(h: &Harness, cookie: &str, conversation: Uuid, input: &str) -> Uuid {
    let id = h.send(cookie, conversation, input, Uuid::new_v4()).await;
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(worker::run_worker(h.state.clone(), receiver));
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id=$1")
                .bind(id)
                .fetch_one(&h.state.pool)
                .await
                .unwrap();
            if status == "completed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    stop.send(true).unwrap();
    worker.await.unwrap();
    id
}

// 验证中文 BM25、空结果、访客/飞书身份隔离和原文重建；不评估真实语义模型。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn file_bm25_and_identity_isolation() {
    let h = setup(false).await;
    let cookie = h.login().await;
    let entry = create(
        &h,
        &cookie,
        "project.orbit.database",
        "Orbit 使用 PostgreSQL 保存项目数据",
        "project",
    )
    .await;
    assert_eq!(
        memories::search(&h.state, "admin", "项目数据")
            .await
            .unwrap()[0]
            .id,
        entry.id
    );
    assert!(
        memories::search(&h.state, "admin", "zzzznevermatches")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        memories::search(&h.state, "feishu:someone", "PostgreSQL")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        h.request("GET", "/api/memories", None, json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (_, _, link) = h
        .request(
            "POST",
            "/api/admin/links",
            Some(&cookie),
            json!({"label":"访客","expires_in_seconds":600}),
        )
        .await;
    let token = link["url"]
        .as_str()
        .unwrap()
        .split("#token=")
        .nth(1)
        .unwrap();
    let (_, guest, _) = h
        .request("POST", "/api/auth/exchange", None, json!({"token":token}))
        .await;
    let guest = guest.unwrap();
    assert!(
        h.request("GET", "/api/memories", Some(&guest), json!({}))
            .await
            .2["entries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        h.request(
            "DELETE",
            &format!("/api/memories/{}", entry.id),
            Some(&guest),
            json!({})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let restarted = AppState::new((*h.state.config).clone(), h.state.pool.clone()).unwrap();
    memories::initialize(&restarted).await.unwrap();
    assert_eq!(
        memories::search(&restarted, "admin", "PostgreSQL")
            .await
            .unwrap()[0]
            .id,
        entry.id
    );
    h.close().await;
}

// 真实 pgvector 验证语义独有召回、哈希失效、版本/维度切换和不可用回退；向量是三维夹具。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL 和 pgvector"]
async fn vector_rebuild_version_and_fallback() {
    let h = setup(true).await;
    let cookie = h.login().await;
    let entry = create(&h, &cookie, "activity.weekend", "周末喜欢散步", "episode").await;
    memories::worker::reconcile(&h.state).await.unwrap();
    assert_eq!(
        memories::search(&h.state, "admin", "走一走").await.unwrap()[0].id,
        entry.id
    );
    assert!(
        memories::search(&h.state, "admin", "咖啡")
            .await
            .unwrap()
            .is_empty()
    );
    // 模拟直接编辑文件，旧向量不得返回更早的内容；补建后新主题可以被检索。
    let path = h
        .state
        .config
        .memory
        .as_ref()
        .unwrap()
        .directory
        .join(auth::hash("admin"))
        .join(format!("{}.md", entry.id));
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("周末喜欢散步", "周末喜欢咖啡")).unwrap();
    assert!(
        memories::search(&h.state, "admin", "走一走")
            .await
            .unwrap()
            .is_empty()
    );
    memories::worker::reconcile(&h.state).await.unwrap();
    assert_eq!(
        memories::search(&h.state, "admin", "咖啡").await.unwrap()[0].content,
        "周末喜欢咖啡"
    );
    let mut config = (*h.state.config).clone();
    config
        .memory
        .as_mut()
        .unwrap()
        .embedding
        .as_mut()
        .unwrap()
        .revision = "2".into();
    let revised = AppState::new(config.clone(), h.state.pool.clone()).unwrap();
    assert!(
        memories::search(&revised, "admin", "走一走")
            .await
            .unwrap()
            .is_empty()
    );
    memories::worker::reconcile(&revised).await.unwrap();
    config
        .memory
        .as_mut()
        .unwrap()
        .embedding
        .as_mut()
        .unwrap()
        .dimensions = 4;
    let mismatched = AppState::new(config, h.state.pool.clone()).unwrap();
    assert!(memories::worker::reconcile(&mismatched).await.is_err());
    assert_eq!(
        memories::search(&mismatched, "admin", "咖啡")
            .await
            .unwrap()[0]
            .id,
        entry.id
    );
    // 关闭语义功能后再遗忘，也必须清掉先前模式留下的向量。
    memories::worker::reconcile(&h.state).await.unwrap();
    let mut disabled_config = (*h.state.config).clone();
    disabled_config.memory.as_mut().unwrap().embedding = None;
    let disabled = AppState::new(disabled_config, h.state.pool.clone()).unwrap();
    memories::forget(&disabled, "admin", entry.id)
        .await
        .unwrap();
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memory_vectors")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
    h.close().await;
}

// 验证只在完成后入队、证据校验和同主题覆盖、跨会话常驻档案；不验证模型实际抽取准确率。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn extraction_evidence_updates_and_context() {
    let h = setup(false).await;
    let cookie = h.login().await;
    let conversation = h.conversation(&cookie).await;
    complete(&h, &cookie, conversation, "以后请用简短中文回答").await;
    assert!(memories::list(&h.state, "admin").await.unwrap().is_empty());
    assert!(memories::worker::extract_one(&h.state).await.unwrap());
    let entries = memories::list(&h.state, "admin").await.unwrap();
    assert_eq!(entries.len(), 1);
    let original = entries[0].id;
    let other = h.conversation(&cookie).await;
    complete(&h, &cookie, other, "无关的新话题").await;
    assert!(
        h.requests.lock().unwrap().last().unwrap()["messages"]
            .to_string()
            .contains("以后请用简短中文回答")
    );
    memories::worker::extract_one(&h.state).await.unwrap();
    complete(&h, &cookie, conversation, "以后请用详细中文回答").await;
    memories::worker::extract_one(&h.state).await.unwrap();
    let updated = memories::list(&h.state, "admin").await.unwrap();
    assert_eq!(updated.len(), 1);
    assert_eq!(updated[0].id, original);
    assert_eq!(updated[0].content, "以后请用详细中文回答");
    complete(&h, &cookie, conversation, "伪造证据测试").await;
    assert!(memories::worker::extract_one(&h.state).await.is_err());
    assert_eq!(memories::list(&h.state, "admin").await.unwrap().len(), 1);
    h.close().await;
}

// 验证遗忘与在途抽取竞争、摘要/近期原文隔离和重启恢复；聊天记录仍保留，不验证备份销毁。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL 和 pgvector"]
async fn forgetting_fences_inflight_jobs_and_history() {
    let h = setup(true).await;
    let cookie = h.login().await;
    let conversation = h.conversation(&cookie).await;
    let entry = create(
        &h,
        &cookie,
        "reply.style",
        "以后请用简短中文回答",
        "profile",
    )
    .await;
    complete(&h, &cookie, conversation, "以后请用简短中文回答").await;
    sqlx::query("UPDATE conversations SET context_summary='以后请用简短中文回答' WHERE id=$1")
        .bind(conversation)
        .execute(&h.state.pool)
        .await
        .unwrap();
    memories::worker::reconcile(&h.state).await.unwrap();
    let state = h.state.clone();
    let pending = tokio::spawn(async move { memories::worker::extract_one(&state).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if h.requests.lock().unwrap().iter().any(|r| {
                r["messages"][0]["content"]
                    .as_str()
                    .unwrap()
                    .contains("长期记忆提取器")
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        h.request(
            "DELETE",
            &format!("/api/memories/{}", entry.id),
            Some(&cookie),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    pending.await.unwrap().unwrap();
    assert!(
        memories::search(&h.state, "admin", "简短中文")
            .await
            .unwrap()
            .is_empty()
    );
    let vectors: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memory_vectors")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(vectors, 0);
    let (summary, through): (String, i64) =
        sqlx::query_as("SELECT context_summary,summary_through FROM conversations WHERE id=$1")
            .bind(conversation)
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert!(summary.is_empty());
    assert!(through > 0);
    complete(&h, &cookie, conversation, "聊个新的话题").await;
    let latest = h.requests.lock().unwrap().last().unwrap().clone();
    assert!(
        !latest["messages"]
            .to_string()
            .contains("以后请用简短中文回答")
    );
    // 回退数据库边界模拟文件提交后数据库提交前崩溃，初始化必须按文件修复。
    sqlx::raw_sql("UPDATE memory_owners SET forgotten_through=0; UPDATE conversations SET summary_through=0,context_summary='陈旧摘要'").execute(&h.state.pool).await.unwrap();
    memories::initialize(&h.state).await.unwrap();
    let restored: String =
        sqlx::query_scalar("SELECT context_summary FROM conversations WHERE id=$1")
            .bind(conversation)
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert!(restored.is_empty());
    h.close().await;
}

// 验证到期过滤、主题去重、更新和损坏文件拒绝；不模拟磁盘硬件故障。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn expiry_edit_and_corrupt_file() {
    let h = setup(false).await;
    let cookie = h.login().await;
    let entry = create(
        &h,
        &cookie,
        "project.deadline",
        "计划周末整理资料",
        "project",
    )
    .await;
    let (status,_,_)=h.request("PUT",&format!("/api/memories/{}",entry.id),Some(&cookie),json!({"key":entry.key,"kind":"project","content":"已经完成资料整理","expires_at":"2020-01-01T00:00:00Z"})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        memories::search(&h.state, "admin", "整理")
            .await
            .unwrap()
            .is_empty()
    );
    let body = json!({"key":entry.key,"kind":"project","content":"重复主题"});
    assert_eq!(
        h.request("POST", "/api/memories", Some(&cookie), body)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let path = h
        .state
        .config
        .memory
        .as_ref()
        .unwrap()
        .directory
        .join(auth::hash("admin"))
        .join(format!("{}.md", entry.id));
    std::fs::write(path, "损坏的记忆元数据").unwrap();
    assert!(memories::list(&h.state, "admin").await.is_err());
    h.close().await;
}

// 验证遗忘等待发送事务后才计算边界；模拟会话锁与插入窗口，不覆盖浏览器输入时序。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn forget_includes_input_committing_under_conversation_lock() {
    let h = setup(false).await;
    let cookie = h.login().await;
    let conversation = h.conversation(&cookie).await;
    let entry = create(
        &h,
        &cookie,
        "private.topic",
        "需要遗忘的测试事实",
        "project",
    )
    .await;
    let mut sending = h.state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM conversations WHERE id=$1 FOR UPDATE")
        .bind(conversation)
        .execute(&mut *sending)
        .await
        .unwrap();
    let state = h.state.clone();
    let deletion = tokio::spawn(async move { memories::forget(&state, "admin", entry.id).await });
    // 等待遗忘进入写入区，随后模拟发送事务仍在插入的新消息。
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if h.state.memory.try_lock().is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO runs(id,conversation_id,idempotency_key,input,batch_id) VALUES($1,$2,$1,'旧事务中的用户输入',$1)")
        .bind(id).bind(conversation).execute(&mut *sending).await.unwrap();
    sending.commit().await.unwrap();
    deletion.await.unwrap().unwrap();
    let (status, seq): (String, i64) = sqlx::query_as("SELECT status,seq FROM runs WHERE id=$1")
        .bind(id)
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    let boundary: i64 = sqlx::query_scalar("SELECT summary_through FROM conversations WHERE id=$1")
        .bind(conversation)
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(status, "cancelled");
    assert!(boundary >= seq);
    h.close().await;
}
