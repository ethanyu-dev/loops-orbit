use super::*;
use axum::{Json, Router, response::IntoResponse, routing::post};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::watch;

/// 本地脚本服务按序返回协议夹具，同时捕获请求以检查预算和上下文。
struct Fixture {
    /// 待测试执行器，只指向回环地址。
    runtime: Runtime,
    /// 各次请求的副本，不连接生产模型。
    requests: Arc<Mutex<Vec<Value>>>,
    /// 测试结束后取消监听，避免遗留后台任务。
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    /// 每个响应指定 MIME 和正文，可混合 SSE 与忽略 stream 的 JSON 回退。
    async fn new(
        responses: Vec<(&'static str, String)>,
        progress: Option<watch::Receiver<String>>,
    ) -> Self {
        let queue = Arc::new(Mutex::new(VecDeque::from(responses)));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                let queue = queue.clone();
                let seen = seen.clone();
                let progress = progress.clone();
                async move {
                    // 每个新模型请求都应清空旧的半截正文，而不是继续追加。
                    if let Some(progress) = progress {
                        assert!(progress.borrow().is_empty());
                    }
                    seen.lock().unwrap().push(body);
                    let (mime, text) = queue.lock().unwrap().pop_front().expect("发生了额外请求");
                    if mime == "fixture/503" {
                        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, text).into_response();
                    }
                    ([(axum::http::header::CONTENT_TYPE, mime)], text).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let runtime = Runtime::new(ModelConfig {
            base_url: format!("http://{address}/v1"),
            model: "fixture".into(),
            api_key: "fixture".into(),
            tools_enabled: true,
            stream_enabled: true,
            chat_output_tokens: DEFAULT_CHAT_OUTPUT_TOKENS,
            stream_max_bytes: crate::DEFAULT_STREAM_MAX_BYTES,
        })
        .unwrap();
        Self {
            runtime,
            requests,
            server,
        }
    }
}

/// 构造非流式响应，同时用于 stream 请求的 JSON 回退测试。
fn answer(reason: &str, message: Value) -> (&'static str, String) {
    (
        "application/json",
        json!({"choices":[{"finish_reason":reason,"message":message}],
        "usage":{"prompt_tokens":100,"completion_tokens":8192}})
        .to_string(),
    )
}

// 验证 JSON 超限仅重发一次且扩大预算、保留上下文；不验证真实模型扩大预算后的成功率。
#[tokio::test]
async fn json_length_retries_current_request_once() {
    let fixture = Fixture::new(
        vec![
            answer("length", json!({"content":"半截"})),
            answer("stop", json!({"content":"完整答案"})),
        ],
        None,
    )
    .await;
    assert_eq!(fixture.runtime.run(&[]).await.unwrap(), "完整答案");
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["max_tokens"], 8192);
    assert_eq!(requests[1]["max_tokens"], 16384);
    assert_eq!(requests[0]["messages"], requests[1]["messages"]);
    assert_eq!(requests[0]["tools"], requests[1]["tools"]);
}

// 验证 SSE 末尾用量、进度清空及 JSON 回退；不覆盖实际公网断流或账单统计。
#[tokio::test]
async fn streaming_length_clears_progress_before_recovery() {
    let (tx, rx) = watch::channel(String::new());
    let stream = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"半截内容\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"completion_tokens\":8192}}\n\n",
        "data: [DONE]\n\n"
    );
    let mut fixture = Fixture::new(
        vec![
            ("text/event-stream", stream.into()),
            answer("stop", json!({"content":"新答案"})),
        ],
        Some(rx),
    )
    .await;
    fixture.runtime.config.chat_output_tokens = 512;
    assert_eq!(
        fixture.runtime.run_stream(&[], Some(&tx)).await.unwrap(),
        "新答案"
    );
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["stream_options"]["include_usage"], true);
    assert_eq!(requests[0]["max_tokens"], 512);
    assert_eq!(requests[1]["max_tokens"], 1024);
    assert!(tx.borrow().is_empty());
}

// 验证第二次仍超限时终止、过滤与未知原因不重试；覆盖 JSON/SSE，不推断线上历史报错原因。
#[tokio::test]
async fn failures_are_bounded_and_classified() {
    for streaming in [false, true] {
        for (reason, code, count) in [
            ("length", "provider_output_limit", 2),
            ("content_filter", "provider_content_filtered", 1),
            ("unsupported", "provider_incomplete_response", 1),
        ] {
            let responses = (0..count)
                .map(|_| {
                    if streaming {
                        (
                            "text/event-stream",
                            format!(
                                "data: {}\n\ndata: [DONE]\n\n",
                                json!({"choices":[{"delta":{},"finish_reason":reason}]})
                            ),
                        )
                    } else {
                        answer(reason, json!({"content":"不可提交"}))
                    }
                })
                .collect();
            let fixture = Fixture::new(responses, None).await;
            let error = fixture.runtime.run(&[]).await.unwrap_err();
            assert_eq!(error.code, code);
            assert!(!error.retryable);
            assert_eq!(fixture.requests.lock().unwrap().len(), count);
        }
    }
}

/// 模拟有副作用的工具，只统计执行次数，不访问第三方系统。
struct WriteHost(AtomicUsize);
impl tools::Host for WriteHost {
    fn instructions(&self) -> String {
        String::new()
    }
    fn definitions(&self) -> Vec<Value> {
        vec![
            json!({"type":"function","function":{"name":"fixture_write","description":"写入夹具",
            "parameters":{"type":"object","properties":{}}}}),
        ]
    }
    fn execute<'a>(
        &'a self,
        _: &'a str,
        _: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send + 'a>> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            json!({"ok":true})
        })
    }
}
/// 工具参数保留字符串形态，以覆盖不完整参数不会触发宿主调用。
fn call(name: &str, arguments: &str) -> Value {
    json!({"tool_calls":[{"id":name,"type":"function","function":{"name":name,"arguments":arguments}}]})
}

// 验证已完成写工具不重放，截断的工具参数不执行；工具副作用仅为计数，不代表真实服务幂等性验收。
#[tokio::test]
async fn recovery_does_not_replay_tools_or_execute_partial_calls() {
    let fixture = Fixture::new(
        vec![
            answer(
                "tool_calls",
                call("tools_load", r#"{"names":["fixture_write"]}"#),
            ),
            answer("tool_calls", call("fixture_write", "{}")),
            answer("length", call("fixture_write", "{")),
            answer("stop", json!({"content":"完成"})),
        ],
        None,
    )
    .await;
    let host = WriteHost(AtomicUsize::new(0));
    assert_eq!(
        fixture
            .runtime
            .run_with_tools(&[], None, Some(&host))
            .await
            .unwrap(),
        "完成"
    );
    assert_eq!(host.0.load(Ordering::SeqCst), 1);
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[2]["messages"], requests[3]["messages"]);
    assert_eq!(requests[2]["tools"], requests[3]["tools"]);
}

// 验证后台摘要维持独立预算、不套用聊天恢复；不覆盖摘要文本质量或数据库调度。
#[tokio::test]
async fn auxiliary_requests_keep_their_budget() {
    let fixture = Fixture::new(vec![answer("length", json!({"content":"半截摘要"}))], None).await;
    assert_eq!(
        fixture.runtime.summarize("", &[]).await.unwrap_err().code,
        "provider_output_limit"
    );
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["max_tokens"], AUXILIARY_OUTPUT_TOKENS);
    assert!(requests[0].get("stream_options").is_none());
}

// 验证启动时预算上下界，避免错误配置进入请求；不连接真实模型检查其特定上限。
#[test]
fn validates_chat_budget() {
    for (budget, valid) in [
        (0, false),
        (255, false),
        (256, true),
        (16384, true),
        (16385, false),
    ] {
        let runtime = Runtime::new(ModelConfig {
            base_url: "http://localhost:1".into(),
            model: "fixture".into(),
            api_key: "fixture".into(),
            tools_enabled: false,
            stream_enabled: false,
            chat_output_tokens: budget,
            stream_max_bytes: crate::DEFAULT_STREAM_MAX_BYTES,
        });
        assert_eq!(runtime.is_ok(), valid);
    }
}

// 验证三种身份实际发给模型的共享表达规则与独立权限/输出约束；固定模型响应不验证真实语言质量。
#[tokio::test]
async fn conversation_style_is_shared_without_sharing_owner_authority() {
    let fixture = Fixture::new(
        vec![
            answer("stop", json!({"content":"你好。"})),
            answer("stop", json!({"content":"不客气。"})),
            answer(
                "stop",
                json!({"content":"{\"answer\":null,\"citations\":[]}"}),
            ),
        ],
        None,
    )
    .await;
    let history = vec![Message {
        role: "user".into(),
        content: "你好".into(),
    }];
    fixture
        .runtime
        .run_for_audience(&history, None, None, false)
        .await
        .unwrap();
    fixture
        .runtime
        .run_for_audience(&history, None, None, true)
        .await
        .unwrap();
    fixture
        .runtime
        .takeover_answer(&json!({"incoming_message":"如何申请","evidence":[]}))
        .await
        .unwrap();
    let requests = fixture.requests.lock().unwrap();
    for request in requests.iter() {
        assert!(
            request["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains(CONVERSATION_STYLE)
        );
    }
    let owner = requests[0]["messages"][0]["content"].as_str().unwrap();
    let external = requests[1]["messages"][0]["content"].as_str().unwrap();
    let delegated = requests[2]["messages"][0]["content"].as_str().unwrap();
    assert!(owner.contains(SYSTEM_PROMPT));
    assert!(!external.contains(SYSTEM_PROMPT));
    assert!(!delegated.contains(SYSTEM_PROMPT));
    assert!(external.contains("已发布通用知识"));
    assert!(delegated.contains("只返回 JSON"));
    assert!(delegated.contains("answer 为 null"));
    assert!(requests[1].get("tools").is_none());
    assert!(requests[2].get("tools").is_none());
}

// 验证网络恢复保留同一模型步骤及已完成工具结果；503/截断流为本地夹具，不覆盖公网稳定性。
#[tokio::test]
async fn transient_recovery_keeps_committed_tool_context() {
    for failure_response in [
        ("fixture/503", "unavailable".into()),
        (
            "text/event-stream",
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n".into(),
        ),
    ] {
        let fixture = Fixture::new(
            vec![
                answer(
                    "tool_calls",
                    call("tools_load", r#"{"names":["fixture_write"]}"#),
                ),
                answer("tool_calls", call("fixture_write", "{}")),
                failure_response,
                answer("stop", json!({"content":"已保存"})),
            ],
            None,
        )
        .await;
        let host = WriteHost(AtomicUsize::new(0));
        assert_eq!(
            fixture
                .runtime
                .run_with_tools(&[], None, Some(&host))
                .await
                .unwrap(),
            "已保存"
        );
        assert_eq!(host.0.load(Ordering::SeqCst), 1);
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[2], requests[3]);
    }
}

// 验证网络恢复耗尽后不请求 worker 重放整轮；工具写入仅为计数，持久化回执由集成测试覆盖。
#[tokio::test]
async fn exhausted_step_never_requests_whole_run_replay() {
    let mut responses = vec![
        answer(
            "tool_calls",
            call("tools_load", r#"{"names":["fixture_write"]}"#),
        ),
        answer("tool_calls", call("fixture_write", "{}")),
    ];
    responses.extend((0..CHAT_REQUEST_ATTEMPTS).map(|_| ("fixture/503", "unavailable".into())));
    let fixture = Fixture::new(responses, None).await;
    let host = WriteHost(AtomicUsize::new(0));
    let error = fixture
        .runtime
        .run_with_tools(&[], None, Some(&host))
        .await
        .unwrap_err();
    assert_eq!(error.code, "provider_rejected");
    assert!(!error.retryable);
    assert_eq!(host.0.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture.requests.lock().unwrap().len(),
        2 + CHAT_REQUEST_ATTEMPTS
    );
}
