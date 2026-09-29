mod stream;
pub mod tools;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fmt, time::Duration};

// 限制工具往返和上游响应大小，防止异常模型无限执行或占满内存。
const MAX_STEPS: usize = 5;
const MAX_TOOL_CALLS: usize = 8;
// 摘要输出独立限长，防止压缩后反而挤占近期对话。
const MAX_SUMMARY_CHARS: usize = 6000;
const MAX_RESPONSE_BYTES: usize = 1_048_576;
// 对话与摘要提示词独立维护，摘要调用不获得工具权限。
const SUMMARY_PROMPT: &str = include_str!("../prompts/summary.md");
const MEMORY_PROMPT: &str = include_str!("../prompts/memory.md");
const SYSTEM_PROMPT: &str = include_str!("../prompts/system.md");

/// 自研执行器的模型配置；密钥只存在服务端内存和环境变量中。
#[derive(Clone)]
pub struct ModelConfig {
    /// 兼容 Chat Completions 的 API 根路径，通常以 /v1 结尾。
    pub base_url: String,
    /// 实际发送给上游的模型标识。
    pub model: String,
    /// 上游身份凭证，不参与 Debug 或日志输出。
    pub api_key: String,
    /// 可关闭工具字段以兼容只支持纯聊天的代理。
    pub tools_enabled: bool,
    /// 可关闭流式请求以兼容仅支持完整 JSON 的代理。
    pub stream_enabled: bool,
}

/// 持久化层交给执行器的已完成对话；工具消息由执行器在单次运行内维护。
#[derive(Clone, Serialize, Deserialize)]
pub struct Message {
    /// 仅接收服务端产生的 user / assistant 角色。
    pub role: String,
    /// 经过长度约束的消息正文。
    pub content: String,
}

/// 错误只携带稳定分类，避免上游错误正文泄露密钥或对话。
#[derive(Debug)]
pub struct Failure {
    /// 可显示和记录的错误分类。
    pub code: &'static str,
    /// 是否允许持久化队列进行有界重试。
    pub retryable: bool,
}
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code)
    }
}
impl std::error::Error for Failure {}

/// 无会话状态的执行器；多个 worker 可以共享同一个连接池。
#[derive(Clone)]
pub struct Runtime {
    /// 带连接和请求超时的 HTTP 客户端。
    client: reqwest::Client,
    /// 启动时确定的上游配置。
    config: ModelConfig,
}

impl Runtime {
    /// 禁止自动跟随重定向，避免自定义代理将授权头导向其他地址。
    pub fn new(config: ModelConfig) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(110))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self { client, config })
    }

    /// 主循环只编排模型和工具步骤；网络读取、请求构造与工具校验分别维护。
    pub async fn run(&self, history: &[Message]) -> Result<String, Failure> {
        self.run_stream(history, None).await
    }

    /// 使用可覆盖的正文快照通知持久化层，慢消费者不会积压每个 token。
    pub async fn run_stream(
        &self,
        history: &[Message],
        progress: Option<&tokio::sync::watch::Sender<String>>,
    ) -> Result<String, Failure> {
        self.run_with_tools(history, progress, None).await
    }

    /// 工具宿主由服务端显式提供，执行器不会自行获得数据库或网络写权限。
    pub async fn run_with_tools(
        &self,
        history: &[Message],
        progress: Option<&tokio::sync::watch::Sender<String>>,
        host: Option<&dyn tools::Host>,
    ) -> Result<String, Failure> {
        let mut messages = vec![json!({ "role": "system", "content": SYSTEM_PROMPT })];
        if let Some(host) = host {
            messages[0]["content"] = json!(format!("{SYSTEM_PROMPT}\n\n{}", host.instructions()));
        }
        messages.extend(
            history
                .iter()
                .map(|message| json!({ "role": message.role, "content": message.content })),
        );

        for step in 0..MAX_STEPS {
            if let Some(progress) = progress {
                progress.send_replace(String::new());
            }
            let answer = self
                .complete_extra(
                    &messages,
                    progress,
                    self.config.tools_enabled,
                    host.map(|h| h.definitions()).unwrap_or_default(),
                )
                .await?;
            let calls = answer
                .get("tool_calls")
                .and_then(Value::as_array)
                .filter(|calls| !calls.is_empty());

            if let Some(calls) = calls {
                self.append_tool_results(&mut messages, &answer, calls, step, host)
                    .await?;
                continue;
            }

            return answer["content"]
                .as_str()
                .filter(|content| !content.trim().is_empty())
                .map(str::to_owned)
                .ok_or(failure("provider_empty_response", false));
        }
        Err(failure("tool_step_limit", false))
    }

    /// 请求体集中维护兼容协议；关闭工具时完全省略 tools 字段。
    fn request_body(&self, messages: &[Value], stream: bool, tools: bool) -> Value {
        let mut body = json!({
            "model": self.config.model,
            "messages": messages,
            "max_tokens": 2048,
            "stream": stream,
        });
        if tools {
            body["tools"] = json!([{
                "type": "function",
                "function": {
                    "name": "current_time",
                    "description": "返回当前 UTC 时间。",
                    "parameters": {
                        "type": "object",
                        "properties": {},
                        "additionalProperties": false,
                    },
                },
            }]);
        }
        body
    }

    /// 使用独立提示词压缩历史，不暴露工具，不改变用户的明确修正。
    pub async fn summarize(&self, previous: &str, history: &[Message]) -> Result<String, Failure> {
        let messages = vec![
            json!({"role":"system", "content":SUMMARY_PROMPT}),
            json!({"role":"user", "content":json!({"previous_summary":previous, "additional_history":history}).to_string()}),
        ];
        let answer = self.complete(&messages, None, false).await?;
        let summary = answer["content"]
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.chars().count() <= MAX_SUMMARY_CHARS)
            .ok_or(failure("invalid_context_summary", true))?;
        Ok(summary.to_owned())
    }

    /// 抽取器只能返回待校验的数据，没有工具或文件写入权限。
    pub async fn extract_memory(&self, input: &Value) -> Result<Value, Failure> {
        let messages = vec![
            json!({"role":"system","content":MEMORY_PROMPT}),
            json!({"role":"user","content":input.to_string()}),
        ];
        let answer = self.complete(&messages, None, false).await?;
        let text = answer["content"]
            .as_str()
            .ok_or(failure("invalid_memory_extraction", true))?;
        serde_json::from_str(text).map_err(|_| failure("invalid_memory_extraction", true))
    }

    /// 后台回访使用固定目的的独立提示词，只返回数据，不执行工具。
    pub async fn followup_decision(
        &self,
        input: &Value,
        discovery: bool,
    ) -> Result<Value, Failure> {
        let prompt = if discovery {
            include_str!("../prompts/followup_discovery.md")
        } else {
            include_str!("../prompts/followup_check.md")
        };
        let messages = vec![
            json!({"role":"system","content":prompt}),
            json!({"role":"user","content":input.to_string()}),
        ];
        let answer = self.complete(&messages, None, false).await?;
        serde_json::from_str(answer["content"].as_str().unwrap_or(""))
            .map_err(|_| failure("invalid_followup_decision", true))
    }

    /// 沟通整理不提供工具；服务端另外验证发送者和逐字证据。
    pub async fn summarize_communications(&self, input: &Value) -> Result<Value, Failure> {
        let messages = vec![
            json!({"role":"system","content":include_str!("../prompts/communications.md")}),
            json!({"role":"user","content":input.to_string()}),
        ];
        let answer = self.complete(&messages, None, false).await?;
        serde_json::from_str(answer["content"].as_str().unwrap_or(""))
            .map_err(|_| failure("invalid_communication_summary", true))
    }

    /// JSON 回退兼容忽略 stream 参数的代理；声明 SSE 时严格校验结束标志。
    async fn complete(
        &self,
        messages: &[Value],
        progress: Option<&tokio::sync::watch::Sender<String>>,
        tools: bool,
    ) -> Result<Value, Failure> {
        self.complete_extra(messages, progress, tools, Vec::new())
            .await
    }

    /// 附加工具只在显式启用工具时公布，摘要和后台判断不会获得动作权限。
    async fn complete_extra(
        &self,
        messages: &[Value],
        progress: Option<&tokio::sync::watch::Sender<String>>,
        tools: bool,
        extra: Vec<Value>,
    ) -> Result<Value, Failure> {
        let mut body = self.request_body(
            messages,
            progress.is_some() && self.config.stream_enabled,
            tools,
        );
        if tools {
            body["tools"]
                .as_array_mut()
                .expect("工具数组已创建")
                .extend(extra);
        }
        let endpoint = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let mut response = self
            .client
            .post(endpoint)
            .bearer_auth(&self.config.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|_| failure("provider_unreachable", true))?;
        let status = response.status();
        if !status.is_success() {
            let retryable = status.as_u16() == 429 || status.is_server_error();
            return Err(failure("provider_rejected", retryable));
        }

        if response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"))
        {
            let mut decoder = stream::Decoder::default();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| failure("provider_read_failed", true))?
            {
                decoder.push(&chunk, progress)?;
            }
            return decoder.finish();
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| failure("provider_read_failed", true))?
        {
            if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(failure("provider_response_too_large", false));
            }
            bytes.extend_from_slice(&chunk);
        }
        let data: Value =
            serde_json::from_slice(&bytes).map_err(|_| failure("provider_invalid_json", false))?;
        if data
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
            .is_some_and(|reason| !matches!(reason, "stop" | "tool_calls"))
        {
            return Err(failure("provider_incomplete_response", false));
        }
        data.pointer("/choices/0/message")
            .cloned()
            .ok_or(failure("provider_empty_response", false))
    }

    /// 校验单轮工具数量及调用 ID，只追加协议允许的 assistant/tool 字段。
    async fn append_tool_results(
        &self,
        messages: &mut Vec<Value>,
        answer: &Value,
        calls: &[Value],
        step: usize,
        host: Option<&dyn tools::Host>,
    ) -> Result<(), Failure> {
        if !self.config.tools_enabled || calls.len() > MAX_TOOL_CALLS {
            return Err(failure("invalid_tool_calls", false));
        }
        messages.push(json!({
            "role": "assistant",
            "content": answer.get("content").unwrap_or(&Value::Null),
            "tool_calls": calls,
        }));
        for call in calls {
            let id = call["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or(failure("invalid_tool_call_id", false))?;
            let name = call["function"]["name"].as_str().unwrap_or("");
            let args = call["function"]["arguments"].as_str().unwrap_or("");
            let result = if let Some(host) = host.filter(|host| {
                host.definitions()
                    .iter()
                    .any(|tool| tool["function"]["name"] == name)
            }) {
                match serde_json::from_str::<Value>(args) {
                    Ok(args) => host.execute(name, args).await,
                    Err(_) => json!({"error":"invalid_arguments"}),
                }
            } else {
                execute_tool(name, args)
            };
            let known_tool = if name == "current_time" {
                "current_time"
            } else {
                "unknown"
            };
            tracing::info!(step, tool = known_tool, "工具执行完成");
            messages.push(json!({
                "role": "tool",
                "tool_call_id": id,
                "content": result.to_string(),
            }));
        }
        Ok(())
    }
}

/// 白名单分发工具，不允许模型动态选择代码、网络地址或文件路径。
fn execute_tool(name: &str, arguments: &str) -> Value {
    if name != "current_time" {
        return json!({"error":"unknown_tool"});
    }
    match serde_json::from_str::<Value>(arguments) {
        Ok(Value::Object(args)) if args.is_empty() => json!({"utc":Utc::now().to_rfc3339()}),
        _ => json!({"error":"invalid_arguments"}),
    }
}

/// 创建不包含敏感上下文的标准运行错误。
fn failure(code: &'static str, retryable: bool) -> Failure {
    Failure { code, retryable }
}

#[cfg(test)]
mod tests {
    use super::*;
    // 验证工具白名单和参数约束；不覆盖真实模型是否按协议调用工具。
    #[test]
    fn tool_boundary() {
        assert!(execute_tool("current_time", "{}")["utc"].is_string());
        assert_eq!(execute_tool("shell", "{}")["error"], "unknown_tool");
        assert_eq!(
            execute_tool("current_time", "{\"cmd\":\"ls\"}")["error"],
            "invalid_arguments"
        );
    }
    // 本地 HTTP 夹具验证工具循环及下一轮上下文；不代表真实模型服务验收。
    #[tokio::test]
    async fn completes_tool_roundtrip() {
        use axum::{Json, Router, routing::post};
        let app = Router::new().route(
            "/v1/chat/completions",
            post(|Json(body): Json<Value>| async move {
                let has_tool_result = body["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|message| message["role"] == "tool");
                let message = if has_tool_result {
                    json!({ "content": "时间已查询" })
                } else {
                    json!({
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": { "name": "current_time", "arguments": "{}" },
                        }],
                    })
                };
                Json(json!({ "choices": [{ "message": message }] }))
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
        })
        .unwrap();
        assert_eq!(
            runtime
                .run(&[Message {
                    role: "user".into(),
                    content: "现在几点".into()
                }])
                .await
                .unwrap(),
            "时间已查询"
        );
        server.abort();
    }
}
