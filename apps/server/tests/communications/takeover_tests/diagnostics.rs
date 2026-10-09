use super::*;

// 验证低分、恰好达标、正常通过和无效上游概率的真实存储/发送边界；不验证模型语义质量或真实投递。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn review_diagnostics_preserve_draft_score_and_original_threshold() {
    for score in [0.8, 0.9, 0.98, -1.0] {
        let (h, f, server) = setup_takeover().await;
        let cookie = h.login().await;
        connect(&h, &cookie).await;
        add(&h, &cookie).await;
        knowledge(&h, &cookie).await;
        enable(&h, &cookie).await;
        f.lock().unwrap().review_probability = score;
        communications::sync::step(&h.state).await.unwrap();
        takeover::step(&h.state).await.unwrap();
        // 用管理接口更改当前阈值，旧任务仍应展示执行当时的 0.9。
        let settings = snapshot(&h, &cookie).await;
        assert_eq!(h.request("PUT", "/api/communications/takeover", Some(&cookie), json!({"enabled":true,"threshold":0.95,"version":settings["version"],"rules_revision":settings["rules_revision"]})).await.0, StatusCode::OK);
        let (status, _, value) = h
            .request(
                "GET",
                "/api/communications/takeover",
                Some(&cookie),
                Value::Null,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let job = &value["jobs"][0];
        assert_eq!(job["probability"], 0.98);
        assert_eq!(job["decision_threshold"], 0.9);
        assert_eq!(value["settings"]["threshold"], 0.95);
        assert_eq!(
            job["draft_answer"],
            "请提交申请表，填写测试用途，由环境管理员审核。"
        );
        if score < 0.0 {
            assert!(job["review_probability"].is_null());
            assert_eq!(job["status"], "failed");
            assert_eq!(job["reason"], "takeover_judge_unavailable");
        } else {
            assert_eq!(job["review_probability"], score);
            assert_eq!(job["status"], if score < 0.9 { "ignored" } else { "sent" });
        }
        if score < 0.9 {
            assert!(job["answer"].is_null());
            assert!(f.lock().unwrap().sent.is_empty());
            if score >= 0.0 {
                assert_eq!(job["reason"], "answer_not_supported");
            }
        } else {
            assert_eq!(f.lock().unwrap().sent.len(), 1);
            assert!(job["answer"].as_str().unwrap().starts_with("[agent] "));
        }
        server.abort();
        h.close().await;
    }
}

// 验证复核完成前草稿可见、取消后不发送、历史空值及访客不能读取诊断；不覆盖供应商后台日志。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn review_diagnostics_are_admin_only_and_survive_cancelled_review() {
    let (h, f, server) = setup_takeover().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    add(&h, &cookie).await;
    knowledge(&h, &cookie).await;
    enable(&h, &cookie).await;
    let gate = Arc::new(MessageGate {
        arrived: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    f.lock().unwrap().review_gate = Some(gate.clone());
    communications::sync::step(&h.state).await.unwrap();
    let state = h.state.clone();
    let work = tokio::spawn(async move { takeover::step(&state).await.unwrap() });
    tokio::time::timeout(Duration::from_secs(5), gate.arrived.notified())
        .await
        .unwrap();
    let (_, _, value) = h
        .request(
            "GET",
            "/api/communications/takeover",
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert!(value["jobs"][0]["draft_answer"].is_string());
    assert!(value["jobs"][0]["review_probability"].is_null());
    assert!(value["jobs"][0]["answer"].is_null());
    assert_eq!(
        h.request("GET", "/api/communications/takeover", None, Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, _, link) = h
        .request(
            "POST",
            "/api/admin/links",
            Some(&cookie),
            json!({"label":"诊断权限测试","expires_in_seconds":3600}),
        )
        .await;
    let url = reqwest::Url::parse(link["url"].as_str().unwrap()).unwrap();
    let token = url.fragment().unwrap().strip_prefix("token=").unwrap();
    let visitor = h
        .request("POST", "/api/auth/exchange", None, json!({"token":token}))
        .await
        .1
        .unwrap();
    let (status, _, body) = h
        .request(
            "GET",
            "/api/communications/takeover",
            Some(&visitor),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.get("jobs").is_none());
    assert_eq!(
        h.request(
            "PUT",
            "/api/communications/takeover",
            Some(&cookie),
            json!({"enabled":false,"threshold":0.9,"version":1})
        )
        .await
        .0,
        StatusCode::OK
    );
    gate.release.notify_one();
    work.await.unwrap();
    assert!(f.lock().unwrap().sent.is_empty());
    let (_, _, value) = h
        .request(
            "GET",
            "/api/communications/takeover",
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(value["jobs"][0]["reason"], "settings_changed");
    assert!(value["jobs"][0]["draft_answer"].is_string());
    // 模拟迁移前已存在的任务，API 必须保留未知值，不能用当前阈值或新草稿回填。
    sqlx::query("UPDATE communication_takeover_jobs SET draft_answer=NULL,review_probability=NULL,decision_threshold=NULL").execute(&h.state.pool).await.unwrap();
    let (_, _, value) = h
        .request(
            "GET",
            "/api/communications/takeover",
            Some(&cookie),
            Value::Null,
        )
        .await;
    for key in ["draft_answer", "review_probability", "decision_threshold"] {
        assert!(value["jobs"][0][key].is_null());
    }
    server.abort();
    h.close().await;
}
