//! 机器人与本人代答共用的消息呈现和投递；身份凭证、目标及发送授权仍由调用者提供。

#[cfg(test)]
mod tests;

use pulldown_cmark::{Event, Options, Parser, Tag};
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

// 标识由服务端添加；历史正文、富文本及卡片回采都保留同一可识别的首行。
pub(crate) const AGENT_PREFIX: &str = "[Agent 自动回复]\n";
// 平台富文本/卡片上限为 30 KB，给平台样式展开与请求封装留出余量。
const RICH_REQUEST_BYTES: usize = 28_000;
// 文本消息上限为 150 KB；超长内容明确失败，不在投递时无声截断。
const TEXT_REQUEST_BYTES: usize = 140_000;
// 包含格式回退在内的总时限，避免延长代答发送期间持有的锁。
const SEND_TIMEOUT: Duration = Duration::from_secs(5);
// 供应商结果仅需状态与消息 ID，不接受无界响应正文。
const RESPONSE_BYTES: usize = 256 * 1024;
// 只有明确的卡片创建失败才允许改用富文本；权限、限流及未知结果不触发回退。
const CARD_REJECTED: i64 = 230099;
// 只生成短摘要，避免通知预览重复整个答案。
const PREVIEW_CHARS: usize = 80;

/// 已经生成并持久化的消息快照，不允许呈现层修改目标或重新调用模型。
pub(crate) struct Outgoing<'a> {
    /// 同一条消息的所有格式尝试共用幂等键，避免未知结果后重复投递。
    pub id: Uuid,
    /// 已持久化的原始 Markdown；代答包含固定标识首行。
    pub content: &'a str,
    /// 主动发送时的明确接收人；回复接口通过 URL 确定被回复消息。
    pub receiver: Option<&'a str>,
    /// 仅本人代答显示小字标识；机器人由平台显示的发送身份识别。
    pub delegated: bool,
}

/// 内容格式仅由服务端决定，模型不能提供卡片组件或交互回调。
#[derive(Clone, Copy)]
enum Format {
    Card,
    Post,
    Text,
}

/// 使用 CommonMark 的源码范围处理扩展标签，代码示例和正常链接不受影响。
/// HTML 以文字展示，Markdown 图片降为链接，避免未上传图片及意外 @ 人导致渲染或通知副作用。
fn display_markdown(source: &str) -> String {
    let mut changes = Vec::new();
    for (event, range) in Parser::new_ext(source, Options::ENABLE_TABLES).into_offset_iter() {
        match event {
            Event::Html(_) | Event::InlineHtml(_) => {
                changes.push((
                    range.clone(),
                    source[range].replace('<', "&lt;").replace('>', "&gt;"),
                ));
            }
            Event::Start(Tag::Image { .. }) => {
                changes.push((range.start..range.start + 1, String::new()));
            }
            _ => {}
        }
    }
    let mut result = String::new();
    let mut cursor = 0;
    for (range, replacement) in changes {
        if range.start >= cursor {
            result.push_str(&source[cursor..range.start]);
            result.push_str(&replacement);
            cursor = range.end;
        }
    }
    result.push_str(&source[cursor..]);
    result
}

/// 将 Markdown 中的可读文字用于聊天列表预览，不让卡片只显示通用的“卡片消息”。
fn preview(source: &str) -> String {
    let mut result = String::new();
    for event in Parser::new(source) {
        match event {
            Event::Text(text) | Event::Code(text) => result.push_str(&text),
            Event::SoftBreak | Event::HardBreak | Event::End(_) => result.push(' '),
            _ => {}
        }
        if result.chars().count() >= PREVIEW_CHARS {
            break;
        }
    }
    result
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(PREVIEW_CHARS)
        .collect()
}

/// 轻量卡片不强制标题或按钮；代答标识与正文分开排版，仍保留首行顺序便于回采识别。
fn request(message: &Outgoing<'_>, format: Format) -> Value {
    let body = if message.delegated {
        message
            .content
            .strip_prefix(AGENT_PREFIX)
            .unwrap_or(message.content)
    } else {
        message.content
    };
    let markdown = display_markdown(body);
    let marker = AGENT_PREFIX.trim_end();
    let (kind, content) = match format {
        Format::Card => {
            let mut elements = vec![];
            if message.delegated {
                elements.push(json!({"tag":"markdown","content":marker,"text_size":"notation"}));
            }
            elements.push(json!({"tag":"markdown","content":markdown}));
            (
                "interactive",
                json!({
                    "schema":"2.0",
                    "config":{"summary":{"content":preview(message.content)}},
                    "body":{"padding":"12px 16px 12px 16px","elements":elements}
                }),
            )
        }
        Format::Post => {
            let mut rows = vec![];
            if message.delegated {
                rows.push(json!([{"tag":"text","text":marker}]));
            }
            rows.push(json!([{"tag":"md","text":markdown}]));
            ("post", json!({"zh_cn":{"title":"","content":rows}}))
        }
        Format::Text => {
            // 文本接口也识别 at/样式标签，降级时转义尖括号，不能重新激活原本已屏蔽的扩展。
            let plain = if message.delegated {
                format!("{AGENT_PREFIX}{body}")
            } else {
                body.to_owned()
            };
            (
                "text",
                json!({"text":plain.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")}),
            )
        }
    };
    let mut value =
        json!({"msg_type":kind,"content":content.to_string(),"uuid":message.id.to_string()});
    if let Some(receiver) = message.receiver {
        value["receive_id"] = json!(receiver);
    }
    value
}

/// 基于最终序列化字节数选择格式；异常转义或长回复以纯文本保留已有正文。
fn bounded_request(message: &Outgoing<'_>, format: Format) -> Result<Value, ()> {
    let rich = request(message, format);
    if rich.to_string().len() <= RICH_REQUEST_BYTES {
        return Ok(rich);
    }
    let plain = request(message, Format::Text);
    if plain.to_string().len() <= TEXT_REQUEST_BYTES {
        Ok(plain)
    } else {
        Err(())
    }
}

/// 有界读取供应商结果，区分明确拒绝和网络未知；错误正文及访问令牌不进入日志。
async fn attempt(
    http: &reqwest::Client,
    url: &reqwest::Url,
    token: &str,
    body: &Value,
) -> Result<(bool, Value), ()> {
    let mut response = http
        .post(url.clone())
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .map_err(|_| ())?;
    let status = response.status();
    if !status.is_success() && status != reqwest::StatusCode::BAD_REQUEST {
        return Err(());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if bytes.len() + chunk.len() > RESPONSE_BYTES {
            return Err(());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
    Ok((status.is_success(), value))
}

/// 两种发送身份共用呈现规则；不切换凭证，不在超时或发送状态未知时另发一条兜底消息。
/// UUID 在卡片和富文本间保持一致，平台在去重窗口内最多接受一条；调用者继续管理原有队列状态。
pub(crate) async fn send(
    http: &reqwest::Client,
    url: reqwest::Url,
    token: &str,
    message: Outgoing<'_>,
) -> Result<Value, ()> {
    tokio::time::timeout(SEND_TIMEOUT, async {
        let body = bounded_request(&message, Format::Card)?;
        let (mut success, mut result) = attempt(http, &url, token, &body).await?;
        if body["msg_type"] == "interactive" && result["code"].as_i64() == Some(CARD_REJECTED) {
            tracing::info!(message_id = %message.id, "飞书卡片创建被拒绝，改用富文本");
            let fallback = bounded_request(&message, Format::Post)?;
            (success, result) = attempt(http, &url, token, &fallback).await?;
        }
        if success
            && result["code"].as_i64() == Some(0)
            && result["data"]["message_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        {
            Ok(result)
        } else {
            Err(())
        }
    })
    .await
    .map_err(|_| ())?
}
