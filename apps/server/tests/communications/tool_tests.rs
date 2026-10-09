use super::*;
use agent_runtime::tools::Host as _;
use orbit_server::tools::Host;

/// 只写隔离数据库与临时目录中的规范 JSONL，构造跨日期资料；不访问真实聊天。
async fn document(h: &Harness, source: Uuid, day: &str, text: &str) -> Uuid {
    let id = Uuid::new_v4();
    let time = chrono::DateTime::parse_from_rfc3339(&format!("{day}T10:00:00+08:00"))
        .unwrap()
        .timestamp_millis();
    let raw = format!(
        "{}\n",
        json!({"message_id":format!("om_{id}"),"chat_id":"oc_fixture","sender_id":"ou_allowed","sender_name":"本人","sender_id_type":"open_id","sender_type":"user","is_me":true,"create_time":time,"update_time":time,"message_type":"text","deleted":false,"text":text,"payload":{}})
    );
    let hash = auth::hash(&raw);
    let dir = h
        .state
        .config
        .memory
        .as_ref()
        .unwrap()
        .directory
        .join("_communications")
        .join(source.to_string())
        .join(id.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{hash}.jsonl")), raw).unwrap();
    sqlx::query("INSERT INTO communication_documents(id,source_id,day,raw_hash,extraction_version) VALUES($1,$2,$3,$4,1)")
        .bind(id).bind(source).bind(day).bind(hash).execute(&h.state.pool).await.unwrap();
    id
}

/// 创建带真实 owner 和有效租约的测试任务，复用生产工具分发入口。
async fn job(h: &Harness, cookie: &str) -> worker::Job {
    let conv = h.conversation(cookie).await;
    let id = h
        .send(cookie, conv, "分析今天处理的内容", Uuid::new_v4())
        .await;
    sqlx::query("UPDATE runs SET status='running',lease_token=$2 WHERE id=$1")
        .bind(id)
        .bind(Uuid::new_v4())
        .execute(&h.state.pool)
        .await
        .unwrap();
    sqlx::query_as("SELECT id,conversation_id,seq,batch_id,input,attempts,lease_token,reply_to FROM runs WHERE id=$1").bind(id).fetch_one(&h.state.pool).await.unwrap()
}

// 验证日期枚举、过滤、分页和参数，且元数据不生成会话地址；不评价真实模型检索选择或最终回复。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn tools_search_dates_pagination_and_keywords() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let first = add(&h, &cookie).await;
    for index in 0..15 {
        let source = if index == 0 {
            first
        } else {
            let source = Uuid::new_v4();
            sqlx::query("INSERT INTO communication_sources(id,owner,chat_id,label,start_at,watermark) VALUES($1,'admin',$2,$3,0,0)")
                .bind(source).bind(format!("oc_{index}")).bind(format!("项目 {index}")).execute(&h.state.pool).await.unwrap();
            source
        };
        document(
            &h,
            source,
            "2026-10-08",
            &format!("已完成项目 {index} 发布，唯一Keyword{index}。"),
        )
        .await;
    }
    document(
        &h,
        first,
        "2026-07-10",
        "今天主要处理了哪些内容，分析下今天。",
    )
    .await;
    let job = job(&h, &cookie).await;
    let host = Host::new(&h.state, &job).await.unwrap();
    let query = json!({"start_day":"2026-10-08","end_day":"2026-10-08","limit":6});
    let page = host.execute("communication_search", query.clone()).await;
    assert_eq!(page["total"], 15, "{page}");
    assert_eq!(page["items"].as_array().unwrap().len(), 6);
    assert_eq!(page["next_offset"], 6);
    assert_eq!(page["scope"]["timezone"], "Asia/Shanghai");
    let mut ids = std::collections::HashSet::new();
    for offset in [0, 6, 12] {
        let mut args = query.clone();
        args["offset"] = json!(offset);
        args["snapshot"] = page["snapshot"].clone();
        let result = host.execute("communication_search", args).await;
        assert_eq!(result["has_more"], offset < 12, "{result}");
        for item in result["items"].as_array().unwrap() {
            assert_eq!(item["day"], "2026-10-08");
            assert!(item.get("source_url").is_none());
            assert!(!item.to_string().contains("/communications?communication="));
            assert!(ids.insert(item["document_id"].as_str().unwrap().to_owned()));
        }
    }
    assert_eq!(ids.len(), 15);
    let keyword = host
        .execute(
            "communication_search",
            json!({"start_day":"2026-10-08","query":"uniquekeyword14"}),
        )
        .await;
    assert_eq!(keyword["total"], 0);
    let keyword = host
        .execute(
            "communication_search",
            json!({"start_day":"2026-10-08","query":"keyword14"}),
        )
        .await;
    assert_eq!(keyword["total"], 1);
    let scoped = host
        .execute(
            "communication_search",
            json!({"start_day":"2026-10-08","source_id":first}),
        )
        .await;
    assert_eq!(scoped["total"], 1);
    for args in [
        json!({"start_day":"2026-02-30"}),
        json!({"start_day":"2026-10-09","end_day":"2026-10-08"}),
        json!({"limit":0}),
        json!({"limit":51}),
        json!({"owner":"admin"}),
        json!({"query":"a".repeat(201)}),
    ] {
        assert_eq!(
            host.execute("communication_search", args).await["error"],
            "invalid_arguments"
        );
    }
    let mut next = query;
    next["offset"] = json!(6);
    next["snapshot"] = page["snapshot"].clone();
    sqlx::query("UPDATE communication_sources SET label='更新名称' WHERE id=$1")
        .bind(first)
        .execute(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(
        host.execute("communication_search", next).await["error"],
        "communication_snapshot_changed"
    );
    server.abort();
    drop(host);
    h.close().await;
}

// 验证原文回退、含业务链接的长文本分页、无会话地址元数据及失效读取；不验证真实模型或飞书投递。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn tools_read_preserves_long_text_and_rejects_stale_files() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    let text = format!(
        "文档 https://example.feishu.cn/wiki/test-doc 网页 https://vercel.com/signup {}",
        "中文😀".repeat(6200)
    );
    let id = document(&h, source, "2026-10-08", &text).await;
    let job = job(&h, &cookie).await;
    let host = Host::new(&h.state, &job).await.unwrap();
    let mut args = json!({"document_id":id,"version":1,"limit":50});
    let mut restored = String::new();
    loop {
        let result = host.execute("communication_read", args.clone()).await;
        assert_eq!(result["mode"], "messages", "{result}");
        assert_eq!(result["summary_fallback"], true);
        assert_eq!(result["coverage"]["message_count"], 1);
        assert!(result["document"].get("source_url").is_none());
        assert_eq!(result["items"][0]["is_me"], true);
        assert!(result.to_string().len() < 26000);
        let items = result["items"].as_array().unwrap();
        assert!(!items.is_empty());
        if restored.is_empty() {
            assert_eq!(result["has_more"], true);
        }
        for item in items {
            restored.push_str(item["text"].as_str().unwrap());
        }
        if result["has_more"] == false {
            break;
        }
        args["offset"] = result["next_offset"].clone();
        args["snapshot"] = result["snapshot"].clone();
    }
    assert_eq!(restored, text);
    let page = host
        .execute(
            "communication_read",
            json!({"document_id":id,"version":1,"offset":1}),
        )
        .await;
    assert_eq!(page["error"], "communication_snapshot_changed");
    sqlx::query("UPDATE communication_documents SET version=2 WHERE id=$1")
        .bind(id)
        .execute(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(
        host.execute("communication_read", args).await["error"],
        "communication_source_changed"
    );
    let hash: String =
        sqlx::query_scalar("SELECT raw_hash FROM communication_documents WHERE id=$1")
            .bind(id)
            .fetch_one(&h.state.pool)
            .await
            .unwrap();
    let path = h
        .state
        .config
        .memory
        .as_ref()
        .unwrap()
        .directory
        .join("_communications")
        .join(source.to_string())
        .join(id.to_string())
        .join(format!("{hash}.jsonl"));
    std::fs::write(path, "损坏内容").unwrap();
    assert_eq!(
        host.execute("communication_read", json!({"document_id":id,"version":2}))
            .await["error"],
        "communication_unavailable"
    );
    let scan = host
        .execute("communication_search", json!({"query":"中文"}))
        .await;
    assert_eq!(scan["coverage"]["unreadable_count"], 1);
    assert_eq!(scan["coverage"]["keyword_scan_complete"], false);
    server.abort();
    drop(host);
    h.close().await;
}

// 验证保留但停用资料的排除统计、未核对范围、账号隔离、注销和任务取消；不覆盖真实供应商权限。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn tools_enforce_identity_source_and_run_boundaries() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    let id = document(&h, source, "2026-10-08", "已有资料").await;
    let job = job(&h, &cookie).await;
    let host = Host::new(&h.state, &job).await.unwrap();
    assert!(
        host.definitions()
            .iter()
            .any(|t| t["function"]["name"] == "communication_search")
    );
    sqlx::query("UPDATE communication_documents SET extraction_version=0 WHERE id=$1")
        .bind(id)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let page = host.execute("communication_search", json!({})).await;
    assert_eq!(page["total"], 0);
    assert_eq!(page["coverage"]["pending_count"], 1);
    sqlx::query("UPDATE communication_documents SET extraction_version=1 WHERE id=$1")
        .bind(id)
        .execute(&h.state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE communication_sources SET enabled=false,subscribed=false WHERE id=$1")
        .bind(source)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let page = host.execute("communication_search", json!({})).await;
    assert_eq!(page["total"], 0);
    assert_eq!(page["coverage"]["excluded_inactive_count"], 1);
    assert_eq!(
        host.execute("communication_read", json!({"document_id":id,"version":1}))
            .await["error"],
        "communication_not_found"
    );
    sqlx::query("UPDATE communication_sources SET enabled=true,subscribed=true WHERE id=$1")
        .bind(source)
        .execute(&h.state.pool)
        .await
        .unwrap();
    for owner in ["guest:unrelated", "feishu:ou_other", "feishu:ou_allowed"] {
        sqlx::query("UPDATE conversations SET owner=$2 WHERE id=$1")
            .bind(job.conversation_id)
            .bind(owner)
            .execute(&h.state.pool)
            .await
            .unwrap();
        let other = Host::new(&h.state, &job).await.unwrap();
        let result = other.execute("communication_search", json!({})).await;
        if owner == "feishu:ou_allowed" {
            assert_eq!(result["total"], 1, "{result}");
        } else {
            assert_eq!(result["error"], "unknown_tool");
            assert!(
                !other
                    .definitions()
                    .iter()
                    .any(|t| t["function"]["name"] == "communication_read")
            );
        }
    }
    sqlx::query("UPDATE conversations SET owner='admin' WHERE id=$1")
        .bind(job.conversation_id)
        .execute(&h.state.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM communication_connections")
        .execute(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(
        host.execute("communication_search", json!({})).await["error"],
        "communication_forbidden"
    );
    sqlx::query("UPDATE runs SET status='cancelled',lease_token=NULL WHERE id=$1")
        .bind(job.id)
        .execute(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(
        host.execute("communication_search", json!({})).await["error"],
        "run_superseded"
    );
    server.abort();
    drop(host);
    h.close().await;
}

// 使用摘要落盘和本地模型夹具验证无会话地址的证据读取与快照更新；不验证真实模型遵守回复约束。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn tools_read_validated_summary_and_detect_summary_changes() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    add(&h, &cookie).await;
    let doc = import(&h, &cookie).await;
    let job = job(&h, &cookie).await;
    let host = Host::new(&h.state, &job).await.unwrap();
    let mut args = json!({"document_id":doc["id"],"version":doc["version"],"limit":1});
    let first = host.execute("communication_read", args.clone()).await;
    assert_eq!(first["mode"], "summary", "{first}");
    assert_eq!(first["summary_fallback"], false);
    assert_eq!(first["items"][0]["is_me"], true);
    assert!(
        first["items"][0]["quote"]
            .as_str()
            .unwrap()
            .contains("材料")
    );
    assert!(first["document"].get("source_url").is_none());
    assert!(!first.to_string().contains("/communications?communication="));
    args["offset"] = first["next_offset"].clone();
    args["snapshot"] = first["snapshot"].clone();
    let next = host.execute("communication_read", args.clone()).await;
    assert_eq!(next["items"][0]["sender"], "小林");
    assert_eq!(next["items"][0]["is_me"], false);
    sqlx::query("UPDATE communication_documents SET summary_hash=NULL WHERE id=$1")
        .bind(Uuid::parse_str(doc["id"].as_str().unwrap()).unwrap())
        .execute(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(
        host.execute("communication_read", args).await["error"],
        "communication_snapshot_changed"
    );
    server.abort();
    drop(host);
    h.close().await;
}

// 本地 Chat Completions 夹具驱动时间→日期查询→原文读取完整往返；只验证协议和数据路径，不验证真实模型规划。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn tools_runtime_roundtrip_resolves_today_and_reads_evidence() {
    let (h, _, server) = setup().await;
    let cookie = h.login().await;
    connect(&h, &cookie).await;
    let source = add(&h, &cookie).await;
    let today = chrono::Utc::now()
        .with_timezone(&chrono_tz::Asia::Shanghai)
        .format("%Y-%m-%d")
        .to_string();
    document(&h, source, &today, "我完成了 AI 网关发布。").await;
    document(&h, source, "2020-07-10", "今天主要处理哪些内容").await;
    let job = job(&h, &cookie).await;
    let host = Host::new(&h.state, &job).await.unwrap();
    let app = Router::new().route("/v1/chat/completions",post(|Json(body):Json<Value>| async move {
        let tools:Vec<Value> = body["messages"].as_array().unwrap().iter().filter(|m|m["role"]=="tool").map(|m|serde_json::from_str(m["content"].as_str().unwrap()).unwrap()).collect();
        let call = |name:&str,args:Value| json!({"tool_calls":[{"id":format!("call_{}",tools.len()),"type":"function","function":{"name":name,"arguments":args.to_string()}}]});
        let message = match tools.len() {
            0 => {
                assert_eq!(body["tools"].as_array().unwrap().len(),3);
                call("current_time",json!({"timezone":"Asia/Shanghai"}))
            },
            1 => call("tools_search",json!({"query":"沟通资料"})),
            2 => call("tools_load",json!({"names":["communication_search","communication_read"]})),
            3 => {
                assert_eq!(tools[0]["timezone"],"Asia/Shanghai");
                call("communication_search",json!({"start_day":tools[0]["local_date"],"end_day":tools[0]["local_date"]}))
            },
            4 => {
                assert_eq!(tools[3]["total"],1,"{}",tools[3]);
                let doc=&tools[3]["items"][0];
                call("communication_read",json!({"document_id":doc["document_id"],"version":doc["version"],"mode":"messages"}))
            },
            5 => {
                assert_eq!(tools[4]["items"][0]["text"],"我完成了 AI 网关发布。");
                assert_eq!(tools[4]["items"][0]["is_me"],true);
                assert_eq!(tools[4]["has_more"],false);
                json!({"content":"已读取今天的网关发布记录"})
            },
            _ => panic!("不应出现额外工具轮次"),
        };
        Json(json!({"choices":[{"message":message}]}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let model = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut config = h.state.config.model.clone();
    config.base_url = format!("http://{address}/v1");
    config.tools_enabled = true;
    let runtime = agent_runtime::Runtime::new(config).unwrap();
    let answer = runtime
        .run_with_tools(
            &[agent_runtime::Message {
                role: "user".into(),
                content: "分析我今天主要处理了哪些内容".into(),
            }],
            None,
            Some(&host),
        )
        .await
        .unwrap();
    assert_eq!(answer, "已读取今天的网关发布记录");
    model.abort();
    server.abort();
    drop(host);
    h.close().await;
}
