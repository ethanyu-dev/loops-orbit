use super::*;

/// 构造同一私聊的模拟原始文字消息；时间位于增量采集窗口内。
fn utterance(f: &TakeoverFixture, id: &str, text: &str, offset: i64, owner: bool) -> Value {
    let mut message = f.message.clone();
    let at = f.message["create_time"]
        .as_str()
        .unwrap()
        .parse::<i64>()
        .unwrap()
        + offset;
    message["message_id"] = json!(id);
    message["sender"]["id"] = json!(if owner { "ou_allowed" } else { "ou_other" });
    message["create_time"] = json!(at.to_string());
    message["update_time"] = json!(at.to_string());
    message["body"]["content"] = json!(json!({"text":text}).to_string());
    message
}
/// 真实管理接口建立已授权、已发布知识的夹具，后续仅控制隔离库时间。
async fn ready() -> (
    Harness,
    Arc<Mutex<TakeoverFixture>>,
    tokio::task::JoinHandle<()>,
    String,
    Uuid,
) {
    let (h, f, server) = setup_takeover().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let id = add(&h, &cookie).await;
    knowledge(&h, &cookie).await;
    enable(&h, &cookie).await;
    (h, f, server, cookie, id)
}
/// 读取管理员会话快照供乐观锁操作。
async fn session(h: &Harness, cookie: &str) -> Value {
    h.request(
        "GET",
        "/api/communications/takeover",
        Some(cookie),
        Value::Null,
    )
    .await
    .2["sessions"][0]
        .clone()
}
/// 操作真实会话 API，返回状态供各测试明确检查冲突和鉴权。
async fn control(
    h: &Harness,
    cookie: Option<&str>,
    id: Uuid,
    mode: &str,
    version: &Value,
) -> StatusCode {
    h.request(
        "PUT",
        &format!("/api/communications/takeover/sessions/{id}"),
        cookie,
        json!({"mode":mode,"version":version}),
    )
    .await
    .0
}
/// 只回拨隔离会话的冷却与恢复边界，消息时间和生产默认值不改变。
async fn release_cooldown(h: &Harness) {
    sqlx::query(
        "UPDATE communication_takeover_sessions SET last_sent_at=now()-interval '6 seconds'",
    )
    .execute(&h.state.pool)
    .await
    .unwrap();
}

// 验证三条输入合为一轮、防抖不提前发送、同片段追问读到已发送历史、冷却后不丢消息；不验收真实模型指代能力。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn batches_inputs_and_preserves_followups_through_cooldown() {
    let (h, f, server, _cookie, _id) = ready().await;
    {
        let mut f = f.lock().unwrap();
        let second = utterance(&f, "om_second", "我想申请测试环境", 100, false);
        let third = utterance(&f, "om_third", "主要用来测试 API", 200, false);
        f.messages = vec![second, third];
    }
    communications::sync::step(&h.state).await.unwrap();
    takeover::step(&h.state).await.unwrap();
    assert!(f.lock().unwrap().sent.is_empty());
    takeover_step(&h.state).await.unwrap();
    {
        let f = f.lock().unwrap();
        assert_eq!(f.sent.len(), 1);
        assert_eq!(f.drafts[0]["pending_messages"].as_array().unwrap().len(), 3);
        assert!(f.drafts[0]["history"].as_array().unwrap().is_empty());
    }
    {
        let mut f = f.lock().unwrap();
        let follow = utterance(&f, "om_follow", "那这个需要填什么？", 300, false);
        f.messages.push(follow);
    }
    due(&h).await;
    communications::sync::step(&h.state).await.unwrap();
    takeover_step(&h.state).await.unwrap();
    assert_eq!(f.lock().unwrap().sent.len(), 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM communication_takeover_jobs WHERE status='queued'"
        )
        .fetch_one(&h.state.pool)
        .await
        .unwrap(),
        1
    );
    release_cooldown(&h).await;
    takeover_step(&h.state).await.unwrap();
    {
        let f = f.lock().unwrap();
        assert_eq!(f.sent.len(), 2);
        assert_eq!(f.drafts[1]["pending_messages"].as_array().unwrap().len(), 1);
        assert_eq!(f.drafts[1]["history"].as_array().unwrap().len(), 4);
        assert_eq!(f.drafts[1]["history"][3]["role"], "assistant");
    }
    server.abort();
    h.close().await;
}

// 验证生成期间的补充使旧任务失效，随后只生成完整新轮次；覆盖发送前回采，不模拟网络已发出后的撤回。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn new_remote_input_supersedes_draft_without_losing_original() {
    let (h, f, server, _cookie, _id) = ready().await;
    let gate = Arc::new(MessageGate {
        arrived: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    f.lock().unwrap().review_gate = Some(gate.clone());
    communications::sync::step(&h.state).await.unwrap();
    let state = h.state.clone();
    let task = tokio::spawn(async move {
        takeover_step(&state).await.unwrap();
    });
    tokio::time::timeout(Duration::from_secs(5), gate.arrived.notified())
        .await
        .unwrap();
    {
        let mut f = f.lock().unwrap();
        let correction = utterance(
            &f,
            "om_correction",
            "更正一下，我只想了解测试用途怎么填写",
            100,
            false,
        );
        f.messages.push(correction);
        f.review_gate = None;
    }
    gate.release.notify_one();
    task.await.unwrap();
    assert!(f.lock().unwrap().sent.is_empty());
    takeover_step(&h.state).await.unwrap();
    {
        let f = f.lock().unwrap();
        assert_eq!(f.sent.len(), 1);
        assert_eq!(f.drafts.len(), 2);
        assert_eq!(f.drafts[1]["pending_messages"].as_array().unwrap().len(), 2);
        assert!(
            f.drafts[1]["incoming_message"]
                .as_str()
                .unwrap()
                .contains("更正一下")
        );
    }
    server.abort();
    h.close().await;
}

// 验证真实本人介入取消旧答案、卡片回采不误认人工、暂停与恢复不补发；不访问真实联系人或平台客户端。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn human_control_is_versioned_and_resume_discards_backlog() {
    let (h, f, server, cookie, id) = ready().await;
    communications::sync::step(&h.state).await.unwrap();
    let before = session(&h, &cookie).await;
    assert_eq!(
        control(&h, None, id, "paused", &before["version"]).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        control(&h, Some(&cookie), id, "human", &before["version"]).await,
        StatusCode::OK
    );
    assert_eq!(
        control(&h, Some(&cookie), id, "auto", &before["version"]).await,
        StatusCode::CONFLICT
    );
    takeover_step(&h.state).await.unwrap();
    assert!(f.lock().unwrap().sent.is_empty());
    let current = session(&h, &cookie).await;
    assert_eq!(current["mode"], "human");
    assert_eq!(
        control(&h, Some(&cookie), id, "paused", &current["version"]).await,
        StatusCode::OK
    );
    // 回拨输入边界来模拟暂停后到达的新消息；仅控制隔离库，不等待真实时间。
    sqlx::query("UPDATE communication_takeover_sessions SET boundary_ms=0")
        .execute(&h.state.pool)
        .await
        .unwrap();
    {
        let mut f = f.lock().unwrap();
        let pending = utterance(&f, "om_paused", "那要怎么提交？", 100, false);
        f.messages.push(pending);
    }
    due(&h).await;
    communications::sync::step(&h.state).await.unwrap();
    assert_eq!(session(&h, &cookie).await["mode"], "paused");
    let current = session(&h, &cookie).await;
    assert_eq!(
        control(&h, Some(&cookie), id, "auto", &current["version"]).await,
        StatusCode::OK
    );
    due(&h).await;
    communications::sync::step(&h.state).await.unwrap();
    takeover_step(&h.state).await.unwrap();
    assert!(f.lock().unwrap().sent.is_empty());
    server.abort();
    h.close().await;
}

// 验证真人介入与发送 ID 防循环、人工状态按双方活动延期且到期先恢复；不覆盖真实平台消息排序误差。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn own_cards_do_not_pause_but_human_activity_does() {
    let (h, f, server, cookie, _id) = ready().await;
    communications::sync::step(&h.state).await.unwrap();
    takeover_step(&h.state).await.unwrap();
    {
        let mut f = f.lock().unwrap();
        // 去掉标识仍应由已确认的发送 ID 识别为 Agent。
        let own = utterance(&f, "om_agent_reply", "平台压缩后的 Agent 正文", 100, true);
        f.messages.push(own);
    }
    due(&h).await;
    communications::sync::step(&h.state).await.unwrap();
    assert_eq!(session(&h, &cookie).await["mode"], "auto");
    {
        let mut f = f.lock().unwrap();
        let human = utterance(&f, "om_real_owner", "这个我来处理", 200, true);
        f.messages.push(human);
    }
    due(&h).await;
    communications::sync::step(&h.state).await.unwrap();
    let human = session(&h, &cookie).await;
    assert_eq!(human["mode"], "human");
    {
        let mut f = f.lock().unwrap();
        let reply = utterance(&f, "om_human_peer", "我再说明一下需求", 300, false);
        f.messages.push(reply);
    }
    due(&h).await;
    communications::sync::step(&h.state).await.unwrap();
    assert!(
        session(&h, &cookie).await["human_until_ms"]
            .as_i64()
            .unwrap()
            > human["human_until_ms"].as_i64().unwrap()
    );
    sqlx::query("UPDATE communication_takeover_sessions SET human_until_ms=0,last_activity_ms=0")
        .execute(&h.state.pool)
        .await
        .unwrap();
    {
        let mut f = f.lock().unwrap();
        let new = utterance(&f, "om_after_idle", "怎么申请测试环境？", 400, false);
        f.messages.push(new);
    }
    due(&h).await;
    communications::sync::step(&h.state).await.unwrap();
    release_cooldown(&h).await;
    takeover_step(&h.state).await.unwrap();
    assert_eq!(session(&h, &cookie).await["mode"], "auto");
    assert_eq!(f.lock().unwrap().sent.len(), 2);
    assert!(
        f.lock().unwrap().drafts.last().unwrap()["history"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    server.abort();
    h.close().await;
}

// 验证取消、道谢静默以及单次澄清分支；模型结果由夹具指定，仅验证协议与复核策略选择。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn cancellation_closure_and_clarification_have_separate_paths() {
    for text in ["不用了", "谢谢！", "这个怎么申请？"] {
        let (h, f, server, _cookie, _id) = ready().await;
        {
            let mut f = f.lock().unwrap();
            let follow = utterance(&f, "om_follow", text, 100, false);
            f.messages.push(follow);
            f.clarify = text == "这个怎么申请？";
        }
        communications::sync::step(&h.state).await.unwrap();
        takeover_step(&h.state).await.unwrap();
        if text == "这个怎么申请？" {
            {
                let f = f.lock().unwrap();
                assert_eq!(f.sent.len(), 1);
                assert!(
                    f.decisions.last().unwrap()["questions"]["answerable"]["instructions"]
                        .as_str()
                        .unwrap()
                        .contains("澄清")
                );
            }
            {
                let mut f = f.lock().unwrap();
                let repeat = utterance(&f, "om_again", "就那个", 200, false);
                f.messages.push(repeat);
            }
            due(&h).await;
            communications::sync::step(&h.state).await.unwrap();
            release_cooldown(&h).await;
            takeover_step(&h.state).await.unwrap();
            assert_eq!(f.lock().unwrap().sent.len(), 1);
            assert_eq!(
                sqlx::query_scalar::<_, String>(
                    "SELECT reason FROM communication_takeover_jobs WHERE message_id='om_again'"
                )
                .fetch_one(&h.state.pool)
                .await
                .unwrap(),
                "clarification_limit"
            );
        } else {
            assert!(f.lock().unwrap().sent.is_empty());
            assert!(f.lock().unwrap().decisions.is_empty());
        }
        server.abort();
        h.close().await;
    }
}

// 验证崩溃遗留投递进入人工核对、后续输入被阻断且重启不重发；不将未知投递当作真实已发送或失败。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn interrupted_dispatch_quarantines_conversation_until_explicit_resume() {
    let (h, f, server, cookie, id) = ready().await;
    communications::sync::step(&h.state).await.unwrap();
    sqlx::query("UPDATE communication_takeover_jobs SET status='dispatching',updated_at=now()-interval '4 minutes'").execute(&h.state.pool).await.unwrap();
    takeover_step(&h.state).await.unwrap();
    takeover_step(&h.state).await.unwrap();
    let current = session(&h, &cookie).await;
    assert_eq!(current["mode"], "uncertain");
    {
        let mut f = f.lock().unwrap();
        let follow = utterance(&f, "om_later", "还在吗？", 100, false);
        f.messages.push(follow);
    }
    due(&h).await;
    communications::sync::step(&h.state).await.unwrap();
    takeover_step(&h.state).await.unwrap();
    assert!(f.lock().unwrap().sent.is_empty());
    let current = session(&h, &cookie).await;
    assert_eq!(
        control(&h, Some(&cookie), id, "auto", &current["version"]).await,
        StatusCode::OK
    );
    takeover_step(&h.state).await.unwrap();
    assert_eq!(session(&h, &cookie).await["mode"], "auto");
    assert!(f.lock().unwrap().sent.is_empty());
    server.abort();
    h.close().await;
}

// 验证未结束的同步窗口禁止领取、并发 worker 只发一条、补充等待有上限；仅测试本地持久化围栏，不承诺平台原子发送。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn pagination_and_concurrent_workers_respect_turn_fences() {
    let (h, f, server, _cookie, id) = ready().await;
    communications::sync::step(&h.state).await.unwrap();
    sqlx::query("UPDATE communication_sources SET window_end=watermark+1 WHERE id=$1")
        .bind(id)
        .execute(&h.state.pool)
        .await
        .unwrap();
    takeover_step(&h.state).await.unwrap();
    assert!(f.lock().unwrap().decisions.is_empty());
    sqlx::query("UPDATE communication_sources SET window_end=NULL WHERE id=$1")
        .bind(id)
        .execute(&h.state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE communication_takeover_turns SET started_at=now()-interval '11 seconds'")
        .execute(&h.state.pool)
        .await
        .unwrap();
    {
        let mut f = f.lock().unwrap();
        let follow = utterance(&f, "om_batch_limit", "需要准备什么？", 100, false);
        f.messages.push(follow);
    }
    due(&h).await;
    communications::sync::step(&h.state).await.unwrap();
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT available_at<=now() FROM communication_takeover_turns WHERE status='pending'"
        )
        .fetch_one(&h.state.pool)
        .await
        .unwrap()
    );
    let (a, b) = tokio::join!(takeover::step(&h.state), takeover::step(&h.state));
    a.unwrap();
    b.unwrap();
    assert_eq!(f.lock().unwrap().sent.len(), 1);
    assert_eq!(f.lock().unwrap().drafts.len(), 1);
    server.abort();
    h.close().await;
}

// 验证两段私聊即使错误复用片段标识也不混入上下文，未知草稿不作为历史；不覆盖模型主动猜测未知事实。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn context_is_scoped_to_chat_and_confirmed_deliveries() {
    let (h, f, server, _cookie, id) = ready().await;
    communications::sync::step(&h.state).await.unwrap();
    takeover_step(&h.state).await.unwrap();
    let other = Uuid::new_v4();
    sqlx::query("INSERT INTO communication_sources(id,owner,chat_id,label,start_at,watermark) SELECT $2,owner,'oc_other','其他联系人',start_at,watermark FROM communication_sources WHERE id=$1")
        .bind(id).bind(other).execute(&h.state.pool).await.unwrap();
    sqlx::query("INSERT INTO communication_takeover_sessions(source_id,source_version,connection_version,connection_generation,settings_version,epoch,boundary_ms) SELECT $2,source_version,connection_version,connection_generation,settings_version,epoch,boundary_ms FROM communication_takeover_sessions WHERE source_id=$1")
        .bind(id).bind(other).execute(&h.state.pool).await.unwrap();
    let turn = Uuid::new_v4();
    sqlx::query("INSERT INTO communication_takeover_turns(id,source_id,epoch,inputs,status) SELECT $2,$3,epoch,'[{\"message_id\":\"om_private\",\"text\":\"其他私聊的私密问题\"}]','closed' FROM communication_takeover_sessions WHERE source_id=$1")
        .bind(id).bind(turn).bind(other).execute(&h.state.pool).await.unwrap();
    sqlx::query("INSERT INTO communication_takeover_jobs(id,source_id,message_id,source_version,connection_version,connection_generation,settings_version,message,turn_id,turn_revision,status,topic,answer) SELECT $2,$3,'om_private',source_version,connection_version,connection_generation,settings_version,message,$4,1,'sent',topic,'其他私聊的私密回答' FROM communication_takeover_jobs WHERE source_id=$1 AND status='sent' LIMIT 1")
        .bind(id).bind(Uuid::new_v4()).bind(other).bind(turn).execute(&h.state.pool).await.unwrap();
    sqlx::query("UPDATE communication_takeover_jobs SET status='unknown',answer='未经确认的回答' WHERE source_id=$1").bind(id).execute(&h.state.pool).await.unwrap();
    {
        let mut f = f.lock().unwrap();
        let follow = utterance(&f, "om_context", "怎么填写测试用途？", 100, false);
        f.messages.push(follow);
    }
    // 只推进正在测试的来源，另一个来源不调用当前聊天的模拟接口。
    sqlx::query("UPDATE communication_sources SET next_sync=CASE WHEN id=$1 THEN now() ELSE now()+interval '1 day' END").bind(id).execute(&h.state.pool).await.unwrap();
    communications::sync::step(&h.state).await.unwrap();
    release_cooldown(&h).await;
    takeover_step(&h.state).await.unwrap();
    let draft = f.lock().unwrap().drafts.last().unwrap().clone();
    assert_eq!(draft["history"], json!([]));
    assert!(!draft.to_string().contains("其他私聊"));
    assert!(!draft.to_string().contains("未经确认"));
    server.abort();
    h.close().await;
}

// 验证来源暂停清除重复存储的正文和上下文，并取消在途轮次；不改变已向平台成功送达的历史消息。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn source_pause_erases_takeover_context_and_cancels_pending_turn() {
    let (h, f, server, cookie, id) = ready().await;
    communications::sync::step(&h.state).await.unwrap();
    takeover_step(&h.state).await.unwrap();
    let version: i64 = sqlx::query_scalar("SELECT version FROM communication_sources WHERE id=$1")
        .bind(id)
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    let status = h
        .request(
            "PUT",
            &format!("/api/communications/sources/{id}"),
            Some(&cookie),
            json!({"enabled":false,"version":version}),
        )
        .await
        .0;
    assert_eq!(status, StatusCode::OK);
    let cleared:bool=sqlx::query_scalar("SELECT bool_and(message='{}'::jsonb AND context IS NULL AND draft_answer IS NULL AND answer IS NULL) FROM communication_takeover_jobs WHERE source_id=$1").bind(id).fetch_one(&h.state.pool).await.unwrap();
    assert!(cleared);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM communication_takeover_inputs")
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(f.lock().unwrap().sent.len(), 1);
    server.abort();
    h.close().await;
}

// 验证迁移前的任务记录仍能去重，不因新轮次表为空而再次处理旧消息；不复刻迁移工具或真实历史数据库。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn legacy_jobs_are_not_reenqueued_after_conversation_upgrade() {
    let (h, f, server, _cookie, id) = ready().await;
    sqlx::query("INSERT INTO communication_takeover_jobs(id,source_id,message_id,source_version,connection_version,connection_generation,settings_version,message,status) SELECT $1,$2,'om_question',1,version,private_discovery_generation,1,'{}','sent' FROM communication_connections WHERE owner='admin'")
        .bind(Uuid::new_v4()).bind(id).execute(&h.state.pool).await.unwrap();
    communications::sync::step(&h.state).await.unwrap();
    takeover_step(&h.state).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM communication_takeover_jobs")
            .fetch_one(&h.state.pool)
            .await
            .unwrap(),
        1
    );
    assert!(f.lock().unwrap().sent.is_empty());
    server.abort();
    h.close().await;
}
