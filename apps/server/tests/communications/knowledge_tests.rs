use super::*;
use orbit_server::knowledge;

// 此文本只用于本地夹具，私人标记应留在原文中而不进入已发布正文。
const RAW: &str = "可复用流程：测试环境请先注册 Vercel，再阅读 https://example.feishu.cn/wiki/shared 。PRIVATE_SOURCE_MARKER：小林曾私下说明。";

/// 固定扫描起点及北京时间，测试不依赖执行机器的当前日期。
async fn schedule(h: &Harness) {
    sqlx::query("UPDATE knowledge_state SET next_scan_day='2026-10-08'")
        .execute(&h.state.pool)
        .await
        .unwrap();
}

/// 仅注入调度时钟，仍走真实每日任务领取与持久化逻辑。
async fn scan(h: &Harness) -> Result<bool, orbit_server::error::ApiError> {
    knowledge::worker::step_at(
        &h.state,
        chrono::DateTime::parse_from_rfc3339("2026-10-09T09:00:00+08:00")
            .unwrap()
            .with_timezone(&chrono::Utc),
    )
    .await
}

/// 用真实访问链接兑换独立访客身份，不以修改请求正文模拟权限。
async fn guest(h: &Harness, cookie: &str) -> String {
    let (_, _, link) = h
        .request(
            "POST",
            "/api/admin/links",
            Some(cookie),
            json!({"label":"知识访客","expires_in_seconds":3600}),
        )
        .await;
    let url = reqwest::Url::parse(link["url"].as_str().unwrap()).unwrap();
    let token = url.fragment().unwrap().strip_prefix("token=").unwrap();
    h.request("POST", "/api/auth/exchange", None, json!({"token":token}))
        .await
        .1
        .unwrap()
}

/// 从真实管理列表获取版本，用同一个写接口确认或撤回。
async fn save(h: &Harness, cookie: &str, entry: &Value, status: &str) -> StatusCode {
    h.request("PUT",&format!("/api/knowledge/{}",entry["id"].as_str().unwrap()),Some(cookie),
        json!({"version":entry["version"],"title":entry["title"],"content":entry["content"],"status":status})).await.0
}

/// 只取最新模型调用；不把夹具生成文字当成真实模型遵守提示词的证明。
fn latest_request(h: &Harness) -> Value {
    h.requests.lock().unwrap().last().unwrap().clone()
}

// 验证原文→候选→本人确认→第三方使用、权限拒绝和撤回清理；不访问真实模型或飞书租户。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn candidates_require_publication_and_hide_private_evidence() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    schedule(&h).await;
    tool_tests::document(&h, source, "2026-10-08", RAW).await;
    assert!(scan(&h).await.unwrap());
    assert!(!scan(&h).await.unwrap());
    let (status, _, data) = h
        .request("GET", "/api/knowledge", Some(&cookie), Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK, "{data}");
    assert_eq!(data["total"], 1);
    let candidate = &data["items"][0];
    assert_eq!(candidate["status"], "candidate");
    assert_eq!(candidate["extraction_day"], "2026-10-08");
    assert!(candidate.get("document_id").is_none());
    assert!(candidate["evidence"][0].get("message_id").is_none());
    assert_eq!(
        candidate["evidence"][0]["quote"],
        "可复用流程：测试环境请先注册 Vercel"
    );
    let visitor = guest(&h, &cookie).await;
    for method in ["GET", "POST"] {
        assert_eq!(
            h.request(
                method,
                "/api/knowledge",
                Some(&visitor),
                if method == "GET" {
                    Value::Null
                } else {
                    json!({"title":"test","content":"test","status":"published"})
                }
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        save(&h, &visitor, candidate, "published").await,
        StatusCode::FORBIDDEN
    );
    let conversation = h.conversation(&visitor).await;
    memory::complete(&h, &visitor, conversation, "我是达达，测试环境怎么注册？").await;
    let first = latest_request(&h);
    assert!(
        first["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("本人以外的提问者")
    );
    assert!(!first.to_string().contains("先注册 Vercel"));
    assert!(!first.to_string().contains("PRIVATE_SOURCE_MARKER"));
    assert_eq!(
        save(&h, &cookie, candidate, "published").await,
        StatusCode::OK
    );
    assert_eq!(
        save(&h, &cookie, candidate, "published").await,
        StatusCode::CONFLICT
    );
    let run = memory::complete(&h, &visitor, conversation, "测试环境怎么注册？").await;
    let published = latest_request(&h);
    assert!(
        published
            .to_string()
            .contains("https://example.feishu.cn/wiki/shared")
    );
    assert!(!published.to_string().contains("PRIVATE_SOURCE_MARKER"));
    assert!(!published.to_string().contains("可复用流程："));
    assert!(!published.to_string().contains("沟通资料检索结果"));
    // 模拟历史回复已使用知识，撤回后仍可查看，但不得再作为新回答的上下文。
    sqlx::query(
        "UPDATE messages SET content='OLD_PUBLIC_SENTINEL' WHERE run_id=$1 AND role='assistant'",
    )
    .bind(run)
    .execute(&h.state.pool)
    .await
    .unwrap();
    let (_, _, data) = h
        .request("GET", "/api/knowledge", Some(&cookie), Value::Null)
        .await;
    assert_eq!(
        save(&h, &cookie, &data["items"][0], "revoked").await,
        StatusCode::OK
    );
    memory::complete(&h, &visitor, conversation, "再说说测试环境怎么注册").await;
    let revoked = latest_request(&h).to_string();
    assert!(!revoked.contains("OLD_PUBLIC_SENTINEL"));
    assert!(!revoked.contains("先注册 Vercel"));
    // 本人继续使用本人模式，第三方模式不能由消息中的身份声称切换。
    let own = h.conversation(&cookie).await;
    memory::complete(&h, &cookie, own, "你好").await;
    assert!(
        latest_request(&h)["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("达达的个人 AI 助理")
    );
    server.abort();
    h.close().await;
}

// 验证来源编辑、暂停、取消订阅、删除和文件损坏不影响独立知识；不验证原文事实真伪或提取召回率。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn source_changes_preserve_independent_approved_knowledge() {
    for scenario in ["edit", "pause", "unsubscribe", "delete", "corrupt"] {
        let (h, _, server) = setup().await;
        let cookie = h.login().await;
        connect(&h, &cookie).await;
        let source = add(&h, &cookie).await;
        schedule(&h).await;
        let doc = tool_tests::document(&h, source, "2026-10-08", RAW).await;
        scan(&h).await.unwrap();
        let (_, _, data) = h
            .request("GET", "/api/knowledge", Some(&cookie), Value::Null)
            .await;
        assert_eq!(
            save(&h, &cookie, &data["items"][0], "published").await,
            StatusCode::OK
        );
        match scenario {
            "edit" => {
                sqlx::query("UPDATE communication_documents SET version=version+1 WHERE id=$1")
                    .bind(doc)
                    .execute(&h.state.pool)
                    .await
                    .unwrap();
            }
            "pause" => {
                sqlx::query("UPDATE communication_sources SET enabled=false WHERE id=$1")
                    .bind(source)
                    .execute(&h.state.pool)
                    .await
                    .unwrap();
            }
            "unsubscribe" => {
                sqlx::query(
                    "UPDATE communication_sources SET subscribed=false,enabled=false WHERE id=$1",
                )
                .bind(source)
                .execute(&h.state.pool)
                .await
                .unwrap();
            }
            "delete" => {
                sqlx::query("DELETE FROM communication_documents WHERE id=$1")
                    .bind(doc)
                    .execute(&h.state.pool)
                    .await
                    .unwrap();
            }
            "corrupt" => {
                let hash: String =
                    sqlx::query_scalar("SELECT raw_hash FROM communication_documents WHERE id=$1")
                        .bind(doc)
                        .fetch_one(&h.state.pool)
                        .await
                        .unwrap();
                std::fs::write(
                    h.state
                        .config
                        .memory
                        .as_ref()
                        .unwrap()
                        .directory
                        .join("_communications")
                        .join(source.to_string())
                        .join(doc.to_string())
                        .join(format!("{hash}.jsonl")),
                    "corrupted",
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        let visitor = guest(&h, &cookie).await;
        let conversation = h.conversation(&visitor).await;
        memory::complete(&h, &visitor, conversation, "测试环境怎么注册？").await;
        assert!(
            latest_request(&h).to_string().contains("先注册 Vercel"),
            "{scenario}"
        );
        let (_, _, data) = h
            .request("GET", "/api/knowledge", Some(&cookie), Value::Null)
            .await;
        assert_eq!(data["total"], 1);
        assert_eq!(data["items"][0]["status"], "published", "{scenario}");
        assert_eq!(
            save(&h, &cookie, &data["items"][0], "published").await,
            StatusCode::OK
        );
        assert!(!scan(&h).await.unwrap(), "原文变化不能重跑当天知识任务");
        server.abort();
        h.close().await;
    }
}

// 验证伪造引文不会落库、失败有限重试，以及显式重试不发布知识；不代表模型必然给出合法 JSON。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn extraction_rejects_fabricated_evidence_with_bounded_retries() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    schedule(&h).await;
    tool_tests::document(&h, source, "2026-10-08", &format!("{RAW} 伪造知识")).await;
    for _ in 0..3 {
        assert_eq!(scan(&h).await.unwrap_err().1, "knowledge_evidence_invalid");
        sqlx::query("UPDATE knowledge_jobs SET available_at=now()")
            .execute(&h.state.pool)
            .await
            .unwrap();
    }
    assert!(!scan(&h).await.unwrap());
    let (_, _, data) = h
        .request("GET", "/api/knowledge", Some(&cookie), Value::Null)
        .await;
    assert_eq!(data["total"], 0);
    assert_eq!(data["jobs"]["failed"], 1);
    assert_eq!(
        h.request("POST", "/api/knowledge/retry", Some(&cookie), Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    let attempts: i32 = sqlx::query_scalar("SELECT attempts FROM knowledge_jobs")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(attempts, 0);
    server.abort();
    h.close().await;
}

// 验证撤回取消在途生成与未开始投递的机器人回复；不能撤回已经发送到飞书的消息。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn withdrawal_cancels_inflight_generation_and_queued_delivery() {
    let h = Harness::new().await;
    let cookie = h.login().await;
    let (_, _, entry) = h
        .request(
            "POST",
            "/api/knowledge",
            Some(&cookie),
            json!({"title":"发布知识","content":"允许对外复用的内容","status":"published"}),
        )
        .await;
    let conversation = h.conversation(&cookie).await;
    let id = h.send(&cookie, conversation, "提问", Uuid::new_v4()).await;
    sqlx::query("UPDATE runs SET available_at=now() WHERE id=$1")
        .bind(id)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let _job = worker::claim(&h.state).await.unwrap().unwrap();
    sqlx::query("UPDATE runs SET knowledge_revision=(SELECT revision FROM knowledge_state),partial_content='旧答案' WHERE id=$1")
        .bind(id)
        .execute(&h.state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO outbox(id,reply_to,content) VALUES($1,'om_test','旧答案')")
        .bind(id)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let changed =
        json!({"id":entry["id"],"version":1,"title":"发布知识","content":"允许对外复用的内容"});
    assert_eq!(save(&h, &cookie, &changed, "revoked").await, StatusCode::OK);
    let run: (String, String) =
        sqlx::query_as("SELECT status,partial_content FROM runs WHERE id=$1")
            .bind(id)
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert_eq!(run, ("cancelled".into(), String::new()));
    let status: String = sqlx::query_scalar("SELECT status FROM outbox WHERE id=$1")
        .bind(id)
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(status, "cancelled");
    h.close().await;
}

// 验证九点边界、全订阅昨天范围、并发幂等及重启续日；不验证真实飞书同步是否及时到达。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn daily_scan_covers_subscriptions_once_after_beijing_nine() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let first = add(&h, &cookie).await;
    schedule(&h).await;
    tool_tests::document(
        &h,
        first,
        "2026-10-08",
        &format!("{RAW} FIRST_SUBSCRIPTION"),
    )
    .await;
    tool_tests::document(&h, first, "2026-10-07", "OLDER_DAY").await;
    tool_tests::document(&h, first, "2026-10-09", "TODAY").await;
    for (label, subscribed, enabled) in [
        ("SECOND_SUBSCRIPTION", true, true),
        ("UNSUBSCRIBED", false, false),
        ("PAUSED", true, false),
    ] {
        let source = Uuid::new_v4();
        sqlx::query("INSERT INTO communication_sources(id,owner,chat_id,label,start_at,watermark,subscribed,enabled,day_timezone) VALUES($1,'admin',$2,$2,0,0,$3,$4,'Asia/Shanghai')")
            .bind(source).bind(label).bind(subscribed).bind(enabled).execute(&h.state.pool).await.unwrap();
        tool_tests::document(&h, source, "2026-10-08", &format!("{RAW} {label}")).await;
    }
    let before = chrono::DateTime::parse_from_rfc3339("2026-10-09T08:59:59+08:00")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(!knowledge::worker::step_at(&h.state, before).await.unwrap());
    assert!(h.requests.lock().unwrap().is_empty());
    // 同一时刻的两个轮询只能领取一次，不能重复调用模型或写入候选。
    let (a, b) = tokio::join!(scan(&h), scan(&h));
    assert_eq!(usize::from(a.unwrap()) + usize::from(b.unwrap()), 1);
    let request = latest_request(&h).to_string();
    for marker in ["FIRST_SUBSCRIPTION", "SECOND_SUBSCRIPTION"] {
        assert!(request.contains(marker));
    }
    for marker in ["OLDER_DAY", "TODAY", "UNSUBSCRIBED", "PAUSED"] {
        assert!(!request.contains(marker));
    }
    assert!(!scan(&h).await.unwrap());
    let job: (String, bool) = sqlx::query_as(
        "SELECT status,messages IS NULL FROM knowledge_jobs WHERE scan_day='2026-10-08'",
    )
    .fetch_one(&h.state.pool)
    .await
    .unwrap();
    assert_eq!(job, ("completed".into(), true));
    let entries: i64 = sqlx::query_scalar("SELECT count(*) FROM knowledge_entries")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(entries, 1, "同一天相同候选正文只入库一次");
    // 持久游标保留漏跑日期；次日九点前仍可补跑已经到期的昨天任务。
    let later = chrono::DateTime::parse_from_rfc3339("2026-10-11T08:00:00+08:00")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(knowledge::worker::step_at(&h.state, later).await.unwrap());
    let days: Vec<String> =
        sqlx::query_scalar("SELECT scan_day::text FROM knowledge_jobs ORDER BY scan_day")
            .fetch_all(&h.state.pool)
            .await
            .unwrap();
    assert_eq!(days, vec!["2026-10-08", "2026-10-09"]);
    assert!(!knowledge::worker::step_at(&h.state, later).await.unwrap());
    server.abort();
    h.close().await;
}

// 验证跨分块重试沿用独立快照，原文删除不打断已开始整理；不测实际模型对跨块内容的归并质量。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn daily_scan_resumes_snapshot_without_source_files() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let first = add(&h, &cookie).await;
    schedule(&h).await;
    tool_tests::document(
        &h,
        first,
        "2026-10-08",
        &format!("{RAW} {}", "x".repeat(13000)),
    )
    .await;
    let second = Uuid::new_v4();
    sqlx::query("INSERT INTO communication_sources(id,owner,chat_id,label,start_at,watermark,day_timezone) VALUES($1,'admin','oc_second','另一订阅',0,0,'Asia/Shanghai')")
        .bind(second).execute(&h.state.pool).await.unwrap();
    tool_tests::document(
        &h,
        second,
        "2026-10-08",
        &format!("{RAW} {}", "y".repeat(13000)),
    )
    .await;
    assert!(scan(&h).await.unwrap());
    let pending: (String, i64, bool) =
        sqlx::query_as("SELECT status,next_offset,messages IS NOT NULL FROM knowledge_jobs")
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert_eq!(pending, ("queued".into(), 1, true));
    sqlx::query("DELETE FROM communication_documents")
        .execute(&h.state.pool)
        .await
        .unwrap();
    assert!(scan(&h).await.unwrap());
    assert!(!scan(&h).await.unwrap());
    assert_eq!(h.requests.lock().unwrap().len(), 2);
    let done: (String, i64, bool) =
        sqlx::query_as("SELECT status,next_offset,messages IS NULL FROM knowledge_jobs")
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert_eq!(done, ("completed".into(), 2, true));
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM knowledge_entries WHERE status='candidate'")
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    server.abort();
    h.close().await;
}

// 验证手选文件立即排队、并发去重、受理后删文件仍能完成和访客拒绝；不使用真实飞书或真实模型。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn manual_extraction_captures_only_selected_file_and_survives_deletion() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    schedule(&h).await;
    let doc = tool_tests::document(
        &h,
        source,
        "2026-10-07",
        &format!("{RAW} ONLY_SELECTED_FILE"),
    )
    .await;
    tool_tests::document(&h, source, "2026-10-08", "DO_NOT_READ_OTHER_DAY").await;
    // 已保留文件可以显式选择；不改变暂停和取消订阅状态。
    sqlx::query("UPDATE communication_sources SET enabled=false,subscribed=false WHERE id=$1")
        .bind(source)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let input = json!({"document_id":doc,"version":1});
    let visitor = guest(&h, &cookie).await;
    assert_eq!(
        h.request(
            "POST",
            "/api/knowledge/extractions",
            Some(&visitor),
            input.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (a, b) = tokio::join!(
        h.request(
            "POST",
            "/api/knowledge/extractions",
            Some(&cookie),
            input.clone()
        ),
        h.request(
            "POST",
            "/api/knowledge/extractions",
            Some(&cookie),
            input.clone()
        )
    );
    assert_eq!(a.0, StatusCode::OK, "{}", a.2);
    assert_eq!(b.0, StatusCode::OK, "{}", b.2);
    assert_eq!(a.2["id"], b.2["id"]);
    assert_eq!(a.2["status"], "queued");
    assert_eq!(a.2["scan_day"], "2026-10-07");
    assert!(h.requests.lock().unwrap().is_empty(), "受理不等待模型执行");
    let path = format!("/api/knowledge/extractions/{}", a.2["id"].as_str().unwrap());
    assert_eq!(
        h.request("GET", &path, Some(&visitor), Value::Null).await.0,
        StatusCode::FORBIDDEN
    );
    sqlx::query("DELETE FROM communication_documents WHERE id=$1")
        .bind(doc)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let before = chrono::DateTime::parse_from_rfc3339("2026-10-09T08:00:00+08:00")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(knowledge::worker::step_at(&h.state, before).await.unwrap());
    assert!(!knowledge::worker::step_at(&h.state, before).await.unwrap());
    let request = latest_request(&h).to_string();
    assert!(request.contains("ONLY_SELECTED_FILE"));
    assert!(!request.contains("DO_NOT_READ_OTHER_DAY"));
    let (_, _, task) = h.request("GET", &path, Some(&cookie), Value::Null).await;
    assert_eq!(task["status"], "completed");
    assert_eq!(task["created_count"], 1);
    assert!(task.get("messages").is_none());
    let (_, _, data) = h
        .request("GET", "/api/knowledge", Some(&cookie), Value::Null)
        .await;
    assert_eq!(data["items"][0]["status"], "candidate");
    assert!(data["items"][0].get("document_id").is_none());
    assert_eq!(
        save(&h, &cookie, &data["items"][0], "published").await,
        StatusCode::OK
    );
    server.abort();
    h.close().await;
}

// 验证手动任务不占用自动扫描日期，重复结果不恢复已拒绝知识；不验证模型跨会话归并的准确性。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn manual_and_daily_jobs_are_independent_and_deduplicate_candidates() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    schedule(&h).await;
    let doc = tool_tests::document(&h, source, "2026-10-08", RAW).await;
    let input = json!({"document_id":doc,"version":1});
    let (_, _, task) = h
        .request(
            "POST",
            "/api/knowledge/extractions",
            Some(&cookie),
            input.clone(),
        )
        .await;
    let before = chrono::DateTime::parse_from_rfc3339("2026-10-09T08:00:00+08:00")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(knowledge::worker::step_at(&h.state, before).await.unwrap());
    let (_, _, data) = h
        .request("GET", "/api/knowledge", Some(&cookie), Value::Null)
        .await;
    assert_eq!(
        save(&h, &cookie, &data["items"][0], "rejected").await,
        StatusCode::OK
    );
    let (_, _, again) = h
        .request("POST", "/api/knowledge/extractions", Some(&cookie), input)
        .await;
    assert_eq!(again["id"], task["id"]);
    assert_eq!(again["status"], "completed");
    assert!(!knowledge::worker::step_at(&h.state, before).await.unwrap());
    assert!(scan(&h).await.unwrap(), "九点自动任务仍须独立运行");
    let jobs: Vec<(String, i64)> =
        sqlx::query_as("SELECT kind,created_count FROM knowledge_jobs ORDER BY kind")
            .fetch_all(&h.state.pool)
            .await
            .unwrap();
    assert_eq!(jobs, vec![("daily".into(), 0), ("manual".into(), 1)]);
    let (_, _, data) = h
        .request("GET", "/api/knowledge", Some(&cookie), Value::Null)
        .await;
    assert_eq!(data["total"], 1);
    assert_eq!(data["items"][0]["status"], "rejected");
    // 文件版本改变后显式手选可创建新任务，不依赖来源变更自动触发。
    sqlx::query("UPDATE communication_documents SET version=version+1 WHERE id=$1")
        .bind(doc)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let (_, _, updated) = h
        .request(
            "POST",
            "/api/knowledge/extractions",
            Some(&cookie),
            json!({"document_id":doc,"version":2}),
        )
        .await;
    assert_ne!(updated["id"], task["id"]);
    assert_eq!(updated["status"], "queued");
    server.abort();
    h.close().await;
}

// 验证陈旧选择、未归一化资料及损坏文件不能受理，并验证手动失败任务可恢复；不评估模型输出质量。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn manual_extraction_validates_selection_and_retries_failed_job() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    schedule(&h).await;
    let doc = tool_tests::document(&h, source, "2026-10-08", &format!("{RAW} 伪造知识")).await;
    assert_eq!(
        h.request(
            "POST",
            "/api/knowledge/extractions",
            Some(&cookie),
            json!({"document_id":doc,"version":2})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    sqlx::query("UPDATE communication_documents SET extraction_version=0 WHERE id=$1")
        .bind(doc)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let input = json!({"document_id":doc,"version":1});
    assert_eq!(
        h.request(
            "POST",
            "/api/knowledge/extractions",
            Some(&cookie),
            input.clone()
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    sqlx::query("UPDATE communication_documents SET extraction_version=1 WHERE id=$1")
        .bind(doc)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM knowledge_jobs")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let (status, _, task) = h
        .request(
            "POST",
            "/api/knowledge/extractions",
            Some(&cookie),
            input.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let before = chrono::DateTime::parse_from_rfc3339("2026-10-09T08:00:00+08:00")
        .unwrap()
        .with_timezone(&chrono::Utc);
    for _ in 0..3 {
        assert_eq!(
            knowledge::worker::step_at(&h.state, before)
                .await
                .unwrap_err()
                .1,
            "knowledge_evidence_invalid"
        );
        sqlx::query("UPDATE knowledge_jobs SET available_at=now()")
            .execute(&h.state.pool)
            .await
            .unwrap();
    }
    let path = format!(
        "/api/knowledge/extractions/{}",
        task["id"].as_str().unwrap()
    );
    let (_, _, failed) = h.request("GET", &path, Some(&cookie), Value::Null).await;
    assert_eq!(failed["status"], "failed");
    let (_, _, retried) = h
        .request(
            "POST",
            "/api/knowledge/extractions",
            Some(&cookie),
            input.clone(),
        )
        .await;
    assert_eq!(retried["id"], task["id"]);
    assert_eq!(retried["status"], "queued");
    // 新版本在受理前损坏必须失败；已受理快照则不再受原文文件影响。
    sqlx::query("UPDATE communication_documents SET version=2,raw_hash=repeat('0',64) WHERE id=$1")
        .bind(doc)
        .execute(&h.state.pool)
        .await
        .unwrap();
    assert_ne!(
        h.request(
            "POST",
            "/api/knowledge/extractions",
            Some(&cookie),
            json!({"document_id":doc,"version":2})
        )
        .await
        .0,
        StatusCode::OK
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM knowledge_jobs")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    server.abort();
    h.close().await;
}

// 验证真实 pgvector 的语义召回、模型版本隔离、编辑撤回及关键词降级；三维夹具不验证真实语义质量。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn rag_vectors_are_persistent_versioned_and_publication_scoped() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    let (_, _, published)=h.request("POST","/api/knowledge",Some(&cookie),json!({"title":"休息建议","content":"散步有助于放松，PUBLIC_RAG_SENTINEL。","tags":["休闲"],"status":"published"})).await;
    let id = Uuid::parse_str(published["id"].as_str().unwrap()).unwrap();
    h.request("POST","/api/knowledge",Some(&cookie),json!({"title":"未批准建议","content":"散步 CANDIDATE_SECRET_SENTINEL","status":"candidate"})).await;
    assert!(
        orbit_server::rag::index::vectors_step(&h.state)
            .await
            .unwrap()
    );
    let indexed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rag_chunks WHERE embedding IS NOT NULL")
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert_eq!(indexed, 1);
    let visitor = guest(&h, &cookie).await;
    let conv = h.conversation(&visitor).await;
    // 查询与正文无相同中文关键词，只有夹具同向量产生召回。
    memory::complete(&h, &visitor, conv, "走一走").await;
    let body = latest_request(&h);
    assert!(body.to_string().contains("PUBLIC_RAG_SENTINEL"));
    assert!(!body.to_string().contains("CANDIDATE_SECRET_SENTINEL"));
    assert!(body.get("tools").is_none());
    // 模型版本不匹配的向量不会进入余弦计算和模型 context。
    sqlx::query("UPDATE rag_chunks SET embedding_version='old-space'")
        .execute(&h.state.pool)
        .await
        .unwrap();
    let conv = h.conversation(&visitor).await;
    memory::complete(&h, &visitor, conv, "走一走").await;
    assert!(
        !latest_request(&h)
            .to_string()
            .contains("PUBLIC_RAG_SENTINEL")
    );
    // 词法索引立即可用，标签参与检索，不依赖网络向量成功。
    let conv = h.conversation(&visitor).await;
    memory::complete(&h, &visitor, conv, "休闲").await;
    assert!(
        latest_request(&h)
            .to_string()
            .contains("PUBLIC_RAG_SENTINEL")
    );
    let (status,_,_)=h.request("PUT",&format!("/api/knowledge/{id}"),Some(&cookie),json!({"version":1,"title":"休息建议","content":"喝咖啡 NEW_PUBLIC_SENTINEL","tags":["饮品"],"status":"published"})).await;
    assert_eq!(status, StatusCode::OK);
    let vectors: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rag_chunks WHERE embedding IS NOT NULL")
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    assert_eq!(vectors, 0);
    assert!(
        orbit_server::rag::index::vectors_step(&h.state)
            .await
            .unwrap()
    );
    let conv = h.conversation(&visitor).await;
    memory::complete(&h, &visitor, conv, "走一走").await;
    assert!(
        !latest_request(&h)
            .to_string()
            .contains("NEW_PUBLIC_SENTINEL")
    );
    let (status,_,_)=h.request("PUT",&format!("/api/knowledge/{id}"),Some(&cookie),json!({"version":2,"title":"休息建议","content":"喝咖啡 NEW_PUBLIC_SENTINEL","status":"revoked"})).await;
    assert_eq!(status, StatusCode::OK);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM rag_chunks")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    server.abort();
    h.close().await;
}

// 验证本人原文召回、第三方数据库范围过滤、快照访问和源文件解耦；不涉及真实飞书身份服务。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn rag_owner_context_and_retained_snapshots_stay_private() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    schedule(&h).await;
    let doc = tool_tests::document(&h, source, "2026-10-08", RAW).await;
    assert!(
        orbit_server::rag::index::documents_step(&h.state)
            .await
            .unwrap()
    );
    assert!(
        !orbit_server::rag::index::documents_step(&h.state)
            .await
            .unwrap()
    );
    assert!(
        orbit_server::rag::index::vectors_step(&h.state)
            .await
            .unwrap()
    );
    scan(&h).await.unwrap();
    let (_, _, data) = h
        .request("GET", "/api/knowledge", Some(&cookie), Value::Null)
        .await;
    let candidate = &data["items"][0];
    let snapshot = candidate["evidence"][0]["snapshot_id"].as_str().unwrap();
    assert_eq!(candidate["evidence"][0]["source_day"], "2026-10-08");
    assert_eq!(
        save(&h, &cookie, candidate, "published").await,
        StatusCode::OK
    );
    let own = h.conversation(&cookie).await;
    memory::complete(&h, &cookie, own, "测试环境怎么注册").await;
    assert!(
        latest_request(&h)
            .to_string()
            .contains("PRIVATE_SOURCE_MARKER")
    );
    let visitor = guest(&h, &cookie).await;
    let conv = h.conversation(&visitor).await;
    memory::complete(&h, &visitor, conv, "测试环境怎么注册").await;
    let public = latest_request(&h).to_string();
    assert!(!public.contains("PRIVATE_SOURCE_MARKER"));
    assert!(!public.contains(snapshot));
    assert_eq!(
        h.request(
            "GET",
            &format!("/api/knowledge/snapshots/{snapshot}"),
            Some(&visitor),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    // 源文件删除清理私有索引，独立快照与已确认知识继续存在。
    sqlx::query("DELETE FROM communication_documents WHERE id=$1")
        .bind(doc)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM rag_chunks WHERE scope='private'")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let (status, _, raw) = h
        .request(
            "GET",
            &format!("/api/knowledge/snapshots/{snapshot}"),
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(raw.to_string().contains("PRIVATE_SOURCE_MARKER"));
    let conv = h.conversation(&visitor).await;
    memory::complete(&h, &visitor, conv, "测试环境怎么注册").await;
    assert!(latest_request(&h).to_string().contains("先注册 Vercel"));
    server.abort();
    h.close().await;
}

// 验证查询 embedding 超时后仍以关键词回答，且后台按条目退避；本地延迟不能代表线上延迟分布。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn rag_slow_embedding_falls_back_without_blocking_answer() {
    let (mut h, _, server) = setup().await;
    let cookie = h.login().await;
    h.request("POST","/api/knowledge",Some(&cookie),json!({"title":"超时回退","content":"检索超时仍可使用 FALLBACK_SENTINEL","status":"published"})).await;
    orbit_server::rag::index::vectors_step(&h.state)
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let slow = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/embeddings",
                post(|| async {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    StatusCode::SERVICE_UNAVAILABLE
                }),
            ),
        )
        .await
        .unwrap();
    });
    let mut config = (*h.state.config).clone();
    let embedding = config.memory.as_mut().unwrap().embedding.as_mut().unwrap();
    embedding.base_url = format!("http://{address}");
    let version = embedding.version();
    h.state = AppState::new(config, h.state.pool.clone()).unwrap();
    h.app = router(h.state.clone());
    // 将现有三维夹具向量标记为此供应商版本，只模拟请求变慢，不测试真实供应商切换。
    sqlx::query("UPDATE rag_chunks SET embedding_version=$1")
        .bind(version)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let visitor = guest(&h, &cookie).await;
    let conv = h.conversation(&visitor).await;
    let started = std::time::Instant::now();
    memory::complete(&h, &visitor, conv, "检索超时").await;
    assert!(started.elapsed() < Duration::from_secs(8));
    assert!(latest_request(&h).to_string().contains("FALLBACK_SENTINEL"));
    slow.abort();
    // 已停止的本地端口立即失败，不等待后台十五秒上限。
    sqlx::query("UPDATE rag_chunks SET embedding_version=NULL")
        .execute(&h.state.pool)
        .await
        .unwrap();
    assert!(
        orbit_server::rag::index::vectors_step(&h.state)
            .await
            .unwrap()
    );
    assert!(
        !orbit_server::rag::index::vectors_step(&h.state)
            .await
            .unwrap()
    );
    let failures: i32 = sqlx::query_scalar("SELECT embedding_failures FROM rag_chunks LIMIT 1")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(failures, 1);
    server.abort();
    h.close().await;
}
