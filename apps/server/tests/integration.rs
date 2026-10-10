mod communications;
mod conversation_flow;
mod followups;
mod linear;
mod memory;
mod output_recovery;
mod todos;

use axum::{
    Json, Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
    response::IntoResponse,
    routing::post,
};
use http_body_util::BodyExt;
use orbit_server::{
    AppState, auth,
    config::{Config, FeishuConfig},
    router, worker,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tower::ServiceExt;
use uuid::Uuid;

// 夹具凭据仅供本地测试，不应复制到生产配置。
const ADMIN: &str = "integration-only-admin-token-32-bytes-long";
const ORIGIN: &str = "http://localhost:5173";

/// 每个测试使用独立 schema，避免清空开发数据库或彼此污染。
struct Harness {
    /// 完整应用状态，包含真实 PostgreSQL 连接。
    state: AppState,
    /// 通过生产路由构造的服务。
    app: Router,
    /// 用于清理隔离 schema 的管理连接。
    admin_pool: PgPool,
    /// 随机生成且仅含安全标识字符的 schema 名称。
    schema: String,
    /// 本地 HTTP 模型夹具句柄。
    model_server: tokio::task::JoinHandle<()>,
    /// 模型收到的请求，用于验证会话上下文，不代表真实供应商行为。
    requests: Arc<Mutex<Vec<Value>>>,
}
impl Harness {
    /// 只有显式提供测试数据库时才运行，不读取项目 .env。
    async fn new() -> Self {
        let url = std::env::var("TEST_DATABASE_URL").expect("请显式设置 TEST_DATABASE_URL");
        let admin_pool = PgPool::connect(&url).await.unwrap();
        let schema = format!("test_{}", Uuid::new_v4().simple());
        // 标识符不能绑定参数；schema 仅由固定前缀和本地 UUID 十六进制组成。
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&admin_pool)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let captured = requests.clone();
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                let captured = captured.clone();
                async move {
                    captured.lock().unwrap().push(body.clone());
                    // 知识夹具只验证结构、证据核对和发布边界，不证明真实模型的提取或隐私判断质量。
                    if body["messages"][0]["content"].as_str().unwrap().contains("通用知识候选提取器") {
                        assert!(body.get("tools").is_none());
                        let input:Value=serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
                        let result:Vec<Value>=input["messages"].as_array().unwrap().iter()
                            .filter(|m|m["text"].as_str().unwrap().contains("可复用流程"))
                            .take(6).map(|m|json!({"title":"测试环境注册流程","content":"测试环境请先注册 Vercel，再阅读 https://example.feishu.cn/wiki/shared 。","evidence":[{"message_id":m["message_id"],"quote":if m["text"].as_str().unwrap().contains("伪造知识") {"并不存在的原文"} else {"可复用流程：测试环境请先注册 Vercel"}}]})).collect();
                        return Json(json!({"choices":[{"message":{"content":json!(result).to_string()}}]})).into_response();
                    }
                    // 图片夹具只验证多模态请求形状，不代表实际模型的识别质量。
                    if body["messages"][0]["content"].as_str().unwrap().contains("阅读聊天消息附带的图片") {
                        assert!(body["messages"][1]["content"][1]["image_url"]["url"].as_str().unwrap().starts_with("data:image/png;base64,"));
                        assert!(body.get("tools").is_none());
                        return Json(json!({"choices":[{"message":{"content":"图片中的订单状态为等待付款，金额 128 元。"}}]})).into_response();
                    }
                    let text = body["messages"].as_array().unwrap().last().unwrap()["content"]
                        .as_str()
                        .unwrap()
                        .to_owned();
                    // 只模拟结构化响应及出处校验，不评价真实模型的归纳能力。
                    if body["messages"][0]["content"].as_str().unwrap().contains("沟通资料整理器") {
                        assert_eq!(body["max_tokens"],8192,"结构化日摘要必须有独立于短聊天的输出预算");
                        let input: Value=serde_json::from_str(&text).unwrap();
                        let items:Vec<Value>=input["messages"].as_array().unwrap().iter().filter(|message| message["text"].as_str().is_some_and(|s|s.contains("材料") || s.contains("散步"))).map(|message| {
                            let quote=message["text"].as_str().unwrap();
                            json!({"kind":if message["is_me"]==true {"my_commitment"} else {"their_commitment"},"text":quote,"message_id":message["message_id"],"quote":if quote.contains("伪造证据") {"原文不存在的证据"} else {quote}})
                        }).collect();
                        return Json(json!({"choices":[{"message":{"content":json!(items).to_string()}}]})).into_response();
                    }
                    if body["messages"][0]["content"].as_str().unwrap().contains("长期记忆提取器") {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        let input: Value = serde_json::from_str(&text).unwrap();
                        let source = input["user_messages"].as_array().unwrap().last().unwrap().as_str().unwrap();
                        let output = if source.contains("以后请用") {
                            json!([{"key":"reply.style","kind":"profile","content":source,"evidence":source,"expires_at":null}])
                        } else if source.contains("伪造证据") {
                            json!([{"key":"invented","kind":"profile","content":"不该保存的事实","evidence":"原文并没有这句话","expires_at":null}])
                        } else { json!([]) };
                        return Json(json!({"choices":[{"message":{"content":output.to_string()}}]})).into_response();
                    }
                    if body["messages"][0]["content"].as_str().unwrap().contains("判断是否适合主动回访") {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        let input: Value = serde_json::from_str(&text).unwrap();
                        let topic = input["followup"]["topic"].as_str().unwrap();
                        let output = if topic == "夹具完成" { json!({"decision":"complete"}) }
                            else if topic == "夹具延后" { json!({"decision":"defer"}) }
                            else if topic == "夹具错误" { json!({"invalid":true}) }
                            else { json!({"decision":"send","message":format!("上次说的{topic}，最近怎么样？")}) };
                        return Json(json!({"choices":[{"message":{"content":output.to_string()}}]})).into_response();
                    }
                    if body["messages"][0]["content"].as_str().unwrap().contains("发现值得回访的事项") {
                        let input: Value = serde_json::from_str(&text).unwrap();
                        let source = input["user_messages"].as_array().unwrap().last().unwrap().as_str().unwrap();
                        let output = if source.contains("尝试新方案") { json!({"topic":"尝试新方案","evidence":"尝试新方案"}) } else { Value::Null };
                        return Json(json!({"choices":[{"message":{"content":output.to_string()}}]})).into_response();
                    }
                    if body["messages"].as_array().unwrap().iter().any(|m|m["role"]=="user" && m["content"]=="夹具提醒：明天交材料") && !body["messages"].as_array().unwrap().iter().any(|m|m["role"]=="tool" && m["tool_call_id"]=="fixture-followup-tool") {
                        if !body["tools"].as_array().is_some_and(|tools|tools.iter().any(|tool|tool["function"]["name"]=="followup_create")) {
                            return Json(json!({"choices":[{"message":{"tool_calls":[{"id":"load-followups","type":"function","function":{"name":"tools_load","arguments":"{\"names\":[\"followup_create\"]}"}}]}}]})).into_response();
                        }
                        let text="夹具提醒：明天交材料";

                        return Json(json!({"choices":[{"message":{"content":null,"tool_calls":[{"id":"fixture-followup-tool","type":"function","function":{"name":"followup_create","arguments":json!({"topic":"交材料","due_at":(chrono::Utc::now()+chrono::Duration::days(1)).to_rfc3339(),"evidence":text}).to_string()}}]}}]})).into_response();
                    }
                    // 预算夹具只模拟协议结束原因，不验证真实模型生成质量。
                    if matches!(text.as_str(), "recover-output-limit" | "always-output-limit" | "filtered-output") {
                        let reason = if text == "filtered-output" { "content_filter" }
                            else if text == "recover-output-limit" && body["max_tokens"] == 16384 { "stop" }
                            else { "length" };
                        return Json(json!({"choices":[{"finish_reason":reason,"message":{"content":if reason == "stop" {"恢复后的完整答案"} else {"不能写入历史的半截答案"}}}]})).into_response();
                    }
                    if text == "permanent-failure" {
                        return (StatusCode::BAD_REQUEST, Json(json!({"error":"fixture"}))).into_response();
                    }
                    if text.starts_with("slow-stream") {
                        let frames = vec![
                            format!("data: {}\n\n", json!({"choices":[{"delta":{"content":"先生成的片段"}}]})),
                            format!("data: {}\n\n", json!({"choices":[{"delta":{"content":"，后生成的内容"}}]})),
                            "data: [DONE]\n\n".to_owned(),
                        ];
                        let stream = futures_util::stream::unfold((0, frames), |(index, frames)| async move {
                            if index >= frames.len() { return None; }
                            if index > 0 { tokio::time::sleep(Duration::from_millis(900)).await; }
                            Some((Ok::<_, std::io::Error>(frames[index].clone()), (index + 1, frames)))
                        });
                        return ([("content-type", "text/event-stream")], Body::from_stream(stream)).into_response();
                    }
                    // 摘要夹具保留输入资料，用于追踪边界与事实传递，不模拟模型摘要质量。
                    if body["messages"][0]["content"].as_str().unwrap().contains("合并成一份中文历史摘要") {
                        if text.contains("summary-error") {
                            return (StatusCode::BAD_REQUEST, Json(json!({"error":"fixture"}))).into_response();
                        }
                        return Json(json!({"choices":[{"message":{"content":format!("历史事实：{text}")}}]})).into_response();
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    (
                        StatusCode::OK,
                        Json(
                            json!({"choices":[{"message":{"content":format!("fixture: {text}")}}]}),
                        ),
                    ).into_response()
                }
            }),
        );
        // 三维向量仅用于验证 pgvector 协议、融合和维度隔离，不代表语义质量。
        let upstream = upstream.route(
            "/v1/embeddings",
            post(|Json(body): Json<Value>| async move {
                let text = body["input"].as_str().unwrap();
                let vector = if text.contains("散步") || text.contains("走一走") {
                    vec![1.0, 0.0, 0.0]
                } else if text.contains("咖啡") {
                    vec![0.0, 1.0, 0.0]
                } else {
                    vec![0.0, 0.0, 1.0]
                };
                Json(json!({"data":[{"index":0,"embedding":vector}]}))
            }),
        );
        // 飞书只访问本地协议夹具：首次返回失败，重试记录 UUID 和接收人。
        let deliveries = requests.clone();
        let upstream = upstream.route("/auth/v3/tenant_access_token/internal", post(|| async { Json(json!({"code":0,"tenant_access_token":"local-fixture-token"})) }))
            .route("/im/v1/messages", post(move |Json(body): Json<Value>| {
                let deliveries = deliveries.clone();
                async move {
                    let mut captured = deliveries.lock().unwrap();
                    let retry = captured.iter().any(|v| v["uuid"] == body["uuid"]);
                    captured.push(body);
                    Json(json!({"code":if retry {0} else {999},"data":{"message_id":"fixture-message"}}))
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let model_server = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });
        let config = Config {
            typesafe: None,
            takeover_questions_file: std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../config/takeover-questions.json"),
            linear: None,
            communications: None,
            memory: None,
            followup_timezone: "Asia/Shanghai".into(),
            database_url: url,
            admin_token: ADMIN.into(),
            public_url: ORIGIN.into(),
            api_public_url: "http://localhost:8080".into(),
            port: 0,
            workers: 2,
            model: agent_runtime::ModelConfig {
                base_url: format!("http://127.0.0.1:{port}/v1"),
                model: "fixture-model".into(),
                api_key: "fixture-key".into(),
                tools_enabled: true,
                stream_enabled: true,
                chat_output_tokens: agent_runtime::DEFAULT_CHAT_OUTPUT_TOKENS,
                stream_max_bytes: agent_runtime::DEFAULT_STREAM_MAX_BYTES,
            },
            feishu: Some(FeishuConfig {
                api_base: format!("http://127.0.0.1:{port}"),
                app_id: "fixture-app".into(),
                app_secret: "fixture-secret".into(),
                verification_token: "fixture-verification".into(),
                encrypt_key: "fixture-encrypt-key".into(),
                allowed_users: vec!["ou_allowed".into()],
            }),
        };
        let state = AppState::new(config, pool).unwrap();
        Self {
            app: router(state.clone()),
            state,
            admin_pool,
            schema,
            model_server,
            requests,
        }
    }
    /// 使用实际路由和中间件发起请求，自动保留响应 Cookie 供认证测试使用。
    async fn request(
        &self,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        body: Value,
    ) -> (StatusCode, Option<String>, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("Origin", ORIGIN)
            .header("Content-Type", "application/json");
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let cookie = response
            .headers()
            .get("set-cookie")
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            cookie,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    /// 使用测试根密钥获取真实会话 Cookie。
    async fn login(&self) -> String {
        let (status, cookie, _) = self
            .request("POST", "/api/auth/login", None, json!({"token":ADMIN}))
            .await;
        assert_eq!(status, StatusCode::OK);
        cookie.unwrap()
    }
    /// 经 API 创建受身份约束的会话。
    async fn conversation(&self, cookie: &str) -> Uuid {
        let (status, _, value) = self
            .request("POST", "/api/conversations", Some(cookie), json!({}))
            .await;
        assert_eq!(status, StatusCode::OK);
        Uuid::parse_str(value["id"].as_str().unwrap()).unwrap()
    }
    /// 经 API 提交消息，复用调用方指定的幂等键。
    async fn send(&self, cookie: &str, id: Uuid, input: &str, key: Uuid) -> Uuid {
        let (status, _, value) = self
            .request(
                "POST",
                &format!("/api/conversations/{id}/messages"),
                Some(cookie),
                json!({"content":input,"idempotency_key":key}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        Uuid::parse_str(value["run_id"].as_str().unwrap()).unwrap()
    }
    /// 正常完成时删除本测试创建的 schema，绝不操作其他数据库表。
    async fn close(self) {
        self.model_server.abort();
        if let Some(config) = &self.state.config.memory {
            // 只清理由该夹具创建的临时目录，不读取生产 MEMORY_DIR。
            std::fs::remove_dir_all(&config.directory).unwrap();
        }
        self.state.pool.close().await;
        // 复用夹具生成的 schema，不接受环境变量或请求输入作为 SQL 标识符。
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA {} CASCADE",
            self.schema
        )))
        .execute(&self.admin_pool)
        .await
        .unwrap();
        self.admin_pool.close().await;
    }
}

// 真实数据库验证管理员登录、来源校验、访客隔离、撤销与过期；不覆盖公网 Cookie 和 TLS 配置。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn authentication_and_grant_lifecycle() {
    let h = Harness::new().await;
    assert_eq!(
        h.request("GET", "/api/conversations", None, json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        h.request("POST", "/api/auth/login", None, json!({"token":"wrong"}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let foreign = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header("Content-Type", "application/json")
        .header("Origin", "https://evil.example")
        .body(Body::from(json!({"token":ADMIN}).to_string()))
        .unwrap();
    assert_eq!(
        h.app.clone().oneshot(foreign).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let admin = h.login().await;
    let private = h.conversation(&admin).await;
    let (status, _, link) = h
        .request(
            "POST",
            "/api/admin/links",
            Some(&admin),
            json!({"label":"测试访客","expires_in_seconds":60}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(link["url"].as_str().unwrap().contains("/#token="));
    let token = link["url"]
        .as_str()
        .unwrap()
        .split("#token=")
        .nth(1)
        .unwrap();
    let (status, cookie, _) = h
        .request("POST", "/api/auth/exchange", None, json!({"token":token}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let guest = cookie.unwrap();
    assert_eq!(
        h.request("GET", "/api/admin/links", Some(&guest), json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        h.request(
            "GET",
            &format!("/api/conversations/{private}"),
            Some(&guest),
            json!({})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let guest_conversation = h.conversation(&guest).await;
    let listed = h
        .request("GET", "/api/conversations", Some(&guest), json!({}))
        .await
        .2;
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["id"], guest_conversation.to_string());
    let token_hash: String = sqlx::query_scalar("SELECT token_hash FROM grants LIMIT 1")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_ne!(token_hash, token);
    assert_eq!(token_hash, auth::hash(token));
    let listed = h
        .request("GET", "/api/admin/links", Some(&admin), json!({}))
        .await
        .2;
    assert!(listed[0].get("token_hash").is_none());
    assert!(listed[0].get("url").is_none());
    assert_eq!(
        h.request(
            "DELETE",
            &format!("/api/admin/links/{}", link["id"].as_str().unwrap()),
            Some(&admin),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        h.request("GET", "/api/me", Some(&guest), json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        h.request("POST", "/api/auth/exchange", None, json!({"token":token}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, _, expiring) = h
        .request(
            "POST",
            "/api/admin/links",
            Some(&admin),
            json!({"label":"到期测试","expires_in_seconds":60}),
        )
        .await;
    let token = expiring["url"]
        .as_str()
        .unwrap()
        .split("#token=")
        .nth(1)
        .unwrap();
    let (_, guest, _) = h
        .request("POST", "/api/auth/exchange", None, json!({"token":token}))
        .await;
    sqlx::query("UPDATE grants SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(Uuid::parse_str(expiring["id"].as_str().unwrap()).unwrap())
        .execute(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(
        h.request("GET", "/api/me", guest.as_deref(), json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    h.request("POST", "/api/auth/logout", Some(&admin), json!({}))
        .await;
    assert_eq!(
        h.request("GET", "/api/me", Some(&admin), json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    h.close().await;
}

// 真实 PostgreSQL 验证幂等入队、并发领取、会话串行和过期租约；不模拟跨区域网络分区。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn idempotency_claims_and_lease_recovery() {
    let h = Harness::new().await;
    let admin = h.login().await;
    let conversation = h.conversation(&admin).await;
    let key = Uuid::new_v4();
    let (first, duplicate) = tokio::join!(
        h.send(&admin, conversation, "first", key),
        h.send(&admin, conversation, "first", key)
    );
    assert_eq!(first, duplicate);
    sqlx::query("UPDATE runs SET available_at=now()")
        .execute(&h.state.pool)
        .await
        .unwrap();
    let (a, b) = tokio::join!(worker::claim(&h.state), worker::claim(&h.state));
    let claims: Vec<_> = [a.unwrap(), b.unwrap()].into_iter().flatten().collect();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].id, first);
    sqlx::query("UPDATE runs SET lease_until=now()-interval '1 second' WHERE id=$1")
        .bind(first)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let recovered = worker::claim(&h.state).await.unwrap().unwrap();
    assert_eq!(recovered.id, first);
    assert_eq!(recovered.attempts, 2);
    assert_ne!(recovered.lease_token, claims[0].lease_token);
    let stale = sqlx::query("UPDATE runs SET status='completed' WHERE id=$1 AND lease_token=$2")
        .bind(first)
        .bind(claims[0].lease_token)
        .execute(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(stale.rows_affected(), 0);
    sqlx::query("UPDATE runs SET status='completed' WHERE id=$1 AND lease_token=$2")
        .bind(first)
        .bind(recovered.lease_token)
        .execute(&h.state.pool)
        .await
        .unwrap();
    let second = h.send(&admin, conversation, "second", Uuid::new_v4()).await;
    sqlx::query("UPDATE runs SET available_at=now()")
        .execute(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(worker::claim(&h.state).await.unwrap().unwrap().id, second);
    let (_, _, value) = h
        .request(
            "POST",
            &format!("/api/conversations/{conversation}/messages"),
            Some(&admin),
            json!({"content":"different","idempotency_key":key}),
        )
        .await;
    assert_eq!(value["error"], "idempotency_conflict");
    h.close().await;
}

// 真实队列和本地 HTTP 模型验证两轮上下文、结果持久化及永久失败；不调用真实付费模型。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn worker_persists_ordered_turns_and_failure() {
    let h = Harness::new().await;
    let admin = h.login().await;
    let conversation = h.conversation(&admin).await;
    h.send(&admin, conversation, "first", Uuid::new_v4()).await;
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(worker::run_worker(h.state.clone(), receiver));
    wait_for(&h.state, "completed", 1).await;
    h.send(&admin, conversation, "second", Uuid::new_v4()).await;
    wait_for(&h.state, "completed", 2).await;
    let (_, _, detail) = h
        .request(
            "GET",
            &format!("/api/conversations/{conversation}"),
            Some(&admin),
            json!({}),
        )
        .await;
    let contents: Vec<_> = detail["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["content"].as_str().unwrap())
        .collect();
    assert_eq!(
        contents,
        vec!["first", "fixture: first", "second", "fixture: second"]
    );
    {
        let requests = h.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let messages = requests[1]["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[2]["content"], "fixture: first");
    }
    h.send(&admin, conversation, "permanent-failure", Uuid::new_v4())
        .await;
    wait_for(&h.state, "failed", 1).await;
    let attempts: i32 = sqlx::query_scalar("SELECT attempts FROM runs WHERE status='failed'")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(attempts, 1);
    stop.send(true).unwrap();
    worker.await.unwrap();
    h.close().await;
}

/// 有界等待数据库状态，不用固定长休眠掩盖 worker 卡死。
async fn wait_for(state: &AppState, status: &str, expected: i64) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM runs WHERE status=$1")
                .bind(status)
                .fetch_one(&state.pool)
                .await
                .unwrap();
            if count == expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("worker 未在测试期限内完成");
}

/// 根据飞书协议生成原始正文签名，不经过 JSON 重新序列化。
fn signed_headers(body: &[u8], timestamp: &str) -> HeaderMap {
    let mut digest = Sha256::new();
    digest.update(timestamp);
    digest.update("fixture-nonce");
    digest.update("fixture-encrypt-key");
    digest.update(body);
    let mut headers = HeaderMap::new();
    headers.insert("x-lark-request-timestamp", timestamp.parse().unwrap());
    headers.insert("x-lark-request-nonce", "fixture-nonce".parse().unwrap());
    headers.insert(
        "x-lark-signature",
        hex::encode(digest.finalize()).parse().unwrap(),
    );
    headers
}

// 验证加密解码、篡改/重放拒绝、飞书重复事件去重与白名单；不代表真实飞书回调或消息发送验收。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn feishu_authentication_and_deduplication() {
    use aes::cipher::{BlockModeEncrypt, KeyIvInit, block_padding::Pkcs7};
    use base64::Engine;
    let h = Harness::new().await;
    let event = json!({
        "schema": "2.0",
        "header": {
            "event_id": "event-1",
            "event_type": "im.message.receive_v1",
            "app_id": "fixture-app",
            "token": "fixture-verification",
        },
        "event": {
            "sender": {
                "sender_type": "user",
                "sender_id": { "open_id": "ou_allowed" },
            },
            "message": {
                "message_id": "message-1",
                "chat_type": "p2p",
                "message_type": "text",
                "content": json!({ "text": "来自飞书" }).to_string(),
            },
        },
    });
    let secret = Sha256::digest(b"fixture-encrypt-key");
    let iv = [9u8; 16];
    let mut encrypted = iv.to_vec();
    encrypted.extend(
        cbc::Encryptor::<aes::Aes256>::new_from_slices(&secret, &iv)
            .unwrap()
            .encrypt_padded_vec::<Pkcs7>(event.to_string().as_bytes()),
    );
    let body =
        json!({"encrypt":base64::engine::general_purpose::STANDARD.encode(encrypted)}).to_string();
    let headers = signed_headers(body.as_bytes(), &chrono::Utc::now().timestamp().to_string());
    assert_eq!(
        orbit_server::feishu::decode(&headers, body.as_bytes(), "fixture-encrypt-key").unwrap(),
        event
    );
    assert!(orbit_server::feishu::decode(&headers, b"{}", "fixture-encrypt-key").is_err());
    let old = signed_headers(body.as_bytes(), "1");
    assert!(orbit_server::feishu::decode(&old, body.as_bytes(), "fixture-encrypt-key").is_err());
    for _ in 0..2 {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/channels/feishu/events")
            .header("Content-Type", "application/json");
        *request.headers_mut().unwrap() = headers.clone();
        let response = h
            .app
            .clone()
            .oneshot(request.body(Body::from(body.clone())).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM runs")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let mut rejected = event.clone();
    rejected["header"]["event_id"] = json!("event-2");
    rejected["event"]["sender"]["sender_id"]["open_id"] = json!("ou_stranger");
    let body = rejected.to_string();
    let headers = signed_headers(body.as_bytes(), &chrono::Utc::now().timestamp().to_string());
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/channels/feishu/events");
    *request.headers_mut().unwrap() = headers;
    assert_eq!(
        h.app
            .clone()
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM runs")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    h.close().await;
}

// 验证平台无签名 challenge 的兼容性及错误令牌拒绝；不覆盖真实控制台的网络可达性。
#[tokio::test]
#[ignore = "需要显式 TEST_DATABASE_URL"]
async fn feishu_challenge_requires_verification_token() {
    let h = Harness::new().await;
    for (token, expected) in [
        ("fixture-verification", StatusCode::OK),
        ("wrong", StatusCode::UNAUTHORIZED),
    ] {
        let body = json!({
            "type": "url_verification",
            "token": token,
            "challenge": "fixture-challenge",
        });
        let request = Request::builder()
            .method("POST")
            .uri("/api/channels/feishu/events")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = h.app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), expected);
        if expected == StatusCode::OK {
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let result: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(result["challenge"], "fixture-challenge");
        }
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM runs")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    h.close().await;
}
