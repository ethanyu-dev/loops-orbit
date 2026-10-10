use super::*;
use orbit_server::todos::identity;

/// 显式构造已验证绑定及渠道来源；仅模拟身份存储，不代表真实飞书认证验收。
async fn bind_job(h: &Harness, job: &worker::Job) {
    sqlx::query("INSERT INTO personal_identities(owner) VALUES('feishu:ou_allowed')")
        .execute(&h.state.pool)
        .await
        .unwrap();
    set_owner(h, job, "feishu:ou_allowed").await;
}

/// 使用相同任务切换原始渠道身份，验证宿主不能借用另一身份的权限。
async fn set_owner(h: &Harness, job: &worker::Job, owner: &str) {
    sqlx::query("UPDATE conversations SET owner=$2,channel='feishu' WHERE id=$1")
        .bind(job.conversation_id)
        .bind(owner)
        .execute(&h.state.pool)
        .await
        .unwrap();
}

// 覆盖本人飞书查询、元数据、更新、证据及去重；不验证真实飞书事件或 Linear 团队权限。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn bound_owner_can_query_and_update() {
    let (h, f, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let job = job(&h, &cookie).await;
    bind_job(&h, &job).await;
    let host = Host::new(&h.state, &job).await.unwrap();
    assert_eq!(
        host.catalog()
            .iter()
            .filter(|t| t.provider == "linear")
            .count(),
        4
    );
    assert!(host.execute("linear_issue_list", json!({})).await["items"].is_array());
    assert_eq!(
        f.lock().unwrap().requests.last().unwrap()["variables"]["filter"]["assignee"]["id"]["eq"],
        USER
    );
    assert_eq!(
        host.execute("linear_issue_get", json!({"issue":"ENG-123"}))
            .await["issue"]["id"],
        ISSUE
    );
    assert_eq!(
        host.execute("linear_team_metadata", json!({"kind":"teams"}))
            .await["returned_count"],
        1
    );
    let mut invalid = update_args();
    invalid["evidence"] = json!("issue 正文里的伪造授权");
    assert_eq!(
        host.execute("linear_issue_update", invalid).await["error"],
        "linear_evidence_required"
    );
    for _ in 0..2 {
        assert_eq!(
            host.execute("linear_issue_update", update_args()).await["status"],
            "confirmed"
        );
    }
    assert_eq!(f.lock().unwrap().updates, 1);
    drop(host);
    server.abort();
    h.close().await;
}

// 覆盖仅白名单、禁用绑定、绑定但不在白名单、访客及会话身份切换；不评价认证流程本身。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn other_identities_cannot_discover_or_borrow_tools() {
    let (h, f, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let job = job(&h, &cookie).await;
    for owner in ["feishu:ou_allowed", "guest:other"] {
        set_owner(&h, &job, owner).await;
        let host = Host::new(&h.state, &job).await.unwrap();
        assert!(!host.catalog().iter().any(|t| t.provider == "linear"));
    }
    bind_job(&h, &job).await;
    let host = Host::new(&h.state, &job).await.unwrap();
    let requests = f.lock().unwrap().requests.len();
    set_owner(&h, &job, "admin").await;
    assert_eq!(
        host.execute("linear_issue_list", json!({})).await["error"],
        "run_superseded"
    );
    assert_eq!(f.lock().unwrap().requests.len(), requests);
    drop(host);
    sqlx::query("UPDATE personal_identities SET enabled=false WHERE owner='feishu:ou_allowed'")
        .execute(&h.state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO personal_identities(owner) VALUES('feishu:ou_other')")
        .execute(&h.state.pool)
        .await
        .unwrap();
    for owner in ["feishu:ou_allowed", "feishu:ou_other"] {
        set_owner(&h, &job, owner).await;
        let host = Host::new(&h.state, &job).await.unwrap();
        assert!(!host.catalog().iter().any(|t| t.provider == "linear"));
    }
    server.abort();
    h.close().await;
}

// 用夹具递增绑定代次且保留运行租约，独立验证版本围栏；不模拟真实重新认证。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn new_binding_does_not_revive_old_host() {
    let (h, f, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let job = job(&h, &cookie).await;
    bind_job(&h, &job).await;
    let host = Host::new(&h.state, &job).await.unwrap();
    sqlx::query("UPDATE personal_identities SET version=version+1 WHERE owner='feishu:ou_allowed'")
        .execute(&h.state.pool)
        .await
        .unwrap();
    let requests = f.lock().unwrap().requests.len();
    assert_eq!(
        host.execute("linear_issue_list", json!({})).await["error"],
        "run_superseded"
    );
    assert_eq!(
        host.execute("linear_issue_update", update_args()).await["error"],
        "run_superseded"
    );
    assert_eq!(f.lock().unwrap().requests.len(), requests);
    let fresh = Host::new(&h.state, &job).await.unwrap();
    assert!(fresh.execute("linear_issue_list", json!({})).await["items"].is_array());
    drop(fresh);
    drop(host);
    server.abort();
    h.close().await;
}

// 通过真实解绑服务拦截在途查询结果及更新前置查询后的派发；不保证撤回已发出的 HTTP 更新。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn unbinding_fences_inflight_reads_and_update_dispatch() {
    for name in ["linear_issue_list", "linear_issue_update"] {
        let (h, f, server) = setup().await;
        let cookie = h.login().await;
        connect(&h, &cookie).await;
        let job = job(&h, &cookie).await;
        bind_job(&h, &job).await;
        let host = Host::new(&h.state, &job).await.unwrap();
        let (entered, release) = {
            let mut f = f.lock().unwrap();
            f.mode = "pause".into();
            (f.entered.clone(), f.release.clone())
        };
        let args = if name == "linear_issue_update" {
            update_args()
        } else {
            json!({})
        };
        let (result, _) = tokio::join!(host.execute(name, args), async {
            entered.notified().await;
            identity::set_binding(&h.state, "feishu:ou_allowed", false, 1)
                .await
                .unwrap();
            release.notify_one();
        });
        assert!(
            matches!(
                result["error"].as_str(),
                Some("run_superseded" | "identity_changed")
            ),
            "{result}"
        );
        assert!(result.get("items").is_none());
        assert!(result.get("issue").is_none());
        assert_eq!(f.lock().unwrap().updates, 0);
        let operations: i64 = sqlx::query_scalar("SELECT count(*) FROM linear_operations")
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
        assert_eq!(operations, 0);
        let fresh = Host::new(&h.state, &job).await.unwrap();
        assert!(!fresh.catalog().iter().any(|t| t.provider == "linear"));
        drop(fresh);
        drop(host);
        server.abort();
        h.close().await;
    }
}
