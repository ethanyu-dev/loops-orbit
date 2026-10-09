use super::{
    Document,
    store::{self, Message},
    unavailable,
};
use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// 分块覆盖整日资料，每块的字节预算限制模型输入；超大单条文本明确计入未处理统计。
const CHUNK_BYTES: usize = 24000;
const MAX_ITEMS_PER_CHUNK: usize = 12;

/// 模型候选经过原文逐条校验后才能保存。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    /// decision / my_commitment / their_commitment / open_question / fact_candidate。
    pub kind: String,
    /// 模型归纳，不能直接当作已核实长期事实。
    pub text: String,
    /// 对应原始消息 ID。
    pub message_id: String,
    /// 原文中连续、逐字匹配的证据。
    pub quote: String,
    /// 以下字段全部由服务器从原始消息填入。
    #[serde(default)]
    pub sender_id: String,
    /// 原始发送时间，毫秒。
    #[serde(default)]
    pub create_time: i64,
    /// 是否本人发送。
    #[serde(default)]
    pub is_me: bool,
}
/// 摘要包含覆盖范围，而不是暗示附件或所有长消息已被理解。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    /// 本次摘要的原文版本。
    pub raw_hash: String,
    /// 全部原文消息数量，包括墓碑和暂不解析的附件。
    pub message_count: usize,
    /// 没有可解析文本或超过单条预算的有效消息数量。
    pub unsupported_count: usize,
    /// 图片机器解读与逐字原话分开保存。
    #[serde(default)]
    pub image_notes: Vec<super::images::Note>,
    /// 已通过证据校验的整理条目.
    pub items: Vec<Item>,
    /// 一次纠错后仍未核验的候选数，不包含已经通过的条目。
    #[serde(default)]
    pub rejected_count: usize,
    /// 未能取得有效候选数组的分块数，不能暗示这些消息已处理。
    #[serde(default)]
    pub failed_chunk_count: usize,
}
/// 校验出处存在、引文精确匹配、承诺归属一致，不声称机器校验能证明语义蕴含。
fn evidence<'a>(item: &Item, messages: &'a [Message]) -> ApiResult<&'a Message> {
    if !matches!(
        item.kind.as_str(),
        "decision" | "my_commitment" | "their_commitment" | "open_question" | "fact_candidate"
    ) || item.text.trim().is_empty()
        || item.text.chars().count() > 500
        || item.quote.trim().is_empty()
        || item.quote.chars().count() > 1000
    {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "communication_summary_invalid_item",
        ));
    }
    let source = messages
        .iter()
        .find(|m| m.message_id == item.message_id && !m.deleted)
        .ok_or(ApiError(
            StatusCode::BAD_GATEWAY,
            "communication_summary_unknown_message",
        ))?;
    if !source.text.contains(&item.quote) {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "communication_summary_quote_mismatch",
        ));
    }
    if (item.kind == "my_commitment" && !source.is_me)
        || (item.kind == "their_commitment" && (source.is_me || source.sender_type != "user"))
    {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "communication_summary_invalid_owner",
        ));
    }
    Ok(source)
}
/// 手工改动文件不能伪造摘要中的身份字段。
pub(crate) fn validate_stored(summary: &Summary, messages: &[Message]) -> ApiResult<()> {
    for item in &summary.items {
        let source = evidence(item, messages)?;
        if source.sender_id != item.sender_id
            || source.create_time != item.create_time
            || source.is_me != item.is_me
        {
            return Err(unavailable("证据身份变化"));
        }
    }
    Ok(())
}
/// 生成阶段只保留稳定分类和可重试标识，不把原文或模型输出写入日志。
pub(super) struct Outcome {
    /// 合格条目及未覆盖范围；空且不完整时不能保存为“未发现事项”。
    pub summary: Summary,
    /// 部分或全部失败的首个原因。
    pub error: Option<&'static str>,
    /// 仅全部失败且属于临时供应商故障时允许重新执行。
    pub retryable: bool,
}
/// 纠错结果必须绑定原候选序号，不能混入未请求的新条目。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Repair {
    /// 初次输出数组中的位置。
    index: usize,
    /// 空值表示无法修复，仍计入未核验范围。
    item: Option<Item>,
}

/// 在锁外生成候选，调用方持锁复核版本后才能写入派生文件。
pub(super) async fn generate(state: &AppState, doc: &Document) -> ApiResult<Outcome> {
    let raw = {
        let _guard = state.communications.lock().await;
        store::raw(state, doc)?
    };
    let mut chunks: Vec<Vec<Message>> = vec![];
    let mut bytes = 0;
    let mut unsupported = 0;
    for message in &raw {
        if message.deleted {
            continue;
        }
        let size = serde_json::to_vec(&model_message(message))
            .map_err(unavailable)?
            .len();
        if message.text.trim().is_empty() || size > CHUNK_BYTES {
            unsupported += 1;
            continue;
        }
        if chunks.is_empty() || bytes + size > CHUNK_BYTES {
            chunks.push(vec![]);
            bytes = 0;
        }
        chunks.last_mut().expect("已有分块").push(message.clone());
        bytes += size;
    }
    let mut outcome = Outcome {
        summary: Summary {
            raw_hash: doc.raw_hash.clone(),
            message_count: raw.len(),
            unsupported_count: unsupported,
            image_notes: vec![],
            items: vec![],
            rejected_count: 0,
            failed_chunk_count: 0,
        },
        error: None,
        retryable: true,
    };
    let chunk_count = chunks.len();
    for (chunk_index, chunk) in chunks.into_iter().enumerate() {
        let input: Vec<Value> = chunk.iter().map(model_message).collect();
        let value = match state
            .runtime
            .summarize_communications(&json!({"messages":input}))
            .await
        {
            Ok(value) => value,
            Err(error) => {
                let transient = error.retryable
                    && matches!(
                        error.code,
                        "provider_rejected" | "provider_unreachable" | "provider_read_failed"
                    );
                outcome.fail_chunk(doc, chunk_index, error.code, transient);
                // 连接、限流或供应商拒绝通常影响整批请求；停止本轮，避免对剩余分块连续冲击上游。
                if matches!(
                    error.code,
                    "provider_rejected" | "provider_unreachable" | "provider_read_failed"
                ) {
                    outcome.summary.failed_chunk_count += chunk_count - chunk_index - 1;
                    break;
                }
                continue;
            }
        };
        let Some(candidates) = value.as_array() else {
            outcome.fail_chunk(
                doc,
                chunk_index,
                "communication_summary_invalid_format",
                false,
            );
            continue;
        };
        if candidates.len() > MAX_ITEMS_PER_CHUNK {
            outcome.fail_chunk(
                doc,
                chunk_index,
                "communication_summary_too_many_items",
                false,
            );
            continue;
        }
        let mut rejected = vec![];
        let mut rejection_code = None;
        for (index, value) in candidates.iter().enumerate() {
            match checked(value.clone(), &chunk) {
                Ok(item) => outcome.summary.items.push(item),
                Err(error) => {
                    log_failure(doc, chunk_index, Some(index), error.1);
                    rejection_code.get_or_insert(error.1);
                    rejected.push(json!({"index":index,"candidate":value,"error":error.1}));
                }
            }
        }
        if rejected.is_empty() {
            continue;
        }
        // 每块最多一次纠错；已通过的条目不会再次发送或被替换。
        let mut unresolved: std::collections::BTreeSet<usize> = rejected
            .iter()
            .map(|v| v["index"].as_u64().expect("候选序号") as usize)
            .collect();
        match state
            .runtime
            .repair_communication_summary(&json!({"messages":input,"rejected":rejected}))
            .await
        {
            Ok(value) => {
                // 整体协议错误不能部分解释，尤其不能以重复序号覆盖已通过的修复。
                if let Ok(repairs) = serde_json::from_value::<Vec<Repair>>(value) {
                    let indices: std::collections::BTreeSet<_> =
                        repairs.iter().map(|r| r.index).collect();
                    if repairs.len() <= MAX_ITEMS_PER_CHUNK
                        && indices.len() == repairs.len()
                        && indices.is_subset(&unresolved)
                    {
                        for repair in repairs {
                            let Some(mut item) = repair.item else {
                                continue;
                            };
                            match hydrate(&mut item, &chunk) {
                                Ok(()) => {
                                    unresolved.remove(&repair.index);
                                    outcome.summary.items.push(item);
                                }
                                Err(error) => {
                                    log_failure(doc, chunk_index, Some(repair.index), error.1)
                                }
                            }
                        }
                    } else {
                        log_failure(
                            doc,
                            chunk_index,
                            None,
                            "communication_summary_invalid_repair",
                        );
                    }
                } else {
                    log_failure(
                        doc,
                        chunk_index,
                        None,
                        "communication_summary_invalid_repair",
                    );
                }
            }
            Err(error) => log_failure(doc, chunk_index, None, error.code),
        }
        if !unresolved.is_empty() {
            outcome.summary.rejected_count += unresolved.len();
            // 分类来自服务端校验，不把候选或供应商正文转为错误消息。
            outcome
                .error
                .get_or_insert(rejection_code.expect("存在未通过核验的候选"));
            outcome.retryable = false;
        }
    }
    Ok(outcome)
}

impl Outcome {
    /// 一个分块失败不抹掉其他分块的合格证据，但必须明确记录覆盖缺口。
    fn fail_chunk(&mut self, doc: &Document, chunk: usize, code: &'static str, retryable: bool) {
        log_failure(doc, chunk, None, code);
        self.summary.failed_chunk_count += 1;
        self.error.get_or_insert(code);
        self.retryable &= retryable;
    }
}
/// 输入和字节预算使用同一结构，正文始终来自保存的原文。
fn model_message(m: &Message) -> Value {
    json!({"message_id":m.message_id,"sender_id":m.sender_id,"sender_name":m.display_name(),
        "sender_type":m.sender_type,"is_me":m.is_me,"create_time":m.create_time,"text":m.text})
}
/// 逐项解析防止单个字段错误吞掉同块中所有合格条目。
fn checked(value: Value, messages: &[Message]) -> ApiResult<Item> {
    let mut item: Item = serde_json::from_value(value).map_err(|_| {
        ApiError(
            StatusCode::BAD_GATEWAY,
            "communication_summary_invalid_format",
        )
    })?;
    hydrate(&mut item, messages)?;
    Ok(item)
}
/// 身份字段完全由原文覆盖，纠错和首次生成遵守同一证据边界。
fn hydrate(item: &mut Item, messages: &[Message]) -> ApiResult<()> {
    let source = evidence(item, messages)?;
    item.sender_id = source.sender_id.clone();
    item.is_me = source.is_me;
    item.create_time = source.create_time;
    Ok(())
}
/// 仅输出定位元数据，不记录候选正文、原文、密钥或供应商响应。
fn log_failure(doc: &Document, chunk: usize, candidate: Option<usize>, code: &'static str) {
    tracing::warn!(document_id=%doc.id, version=doc.version, attempt=doc.summary_attempts,
        chunk, candidate, code, "沟通整理候选未通过核验");
}

#[cfg(test)]
#[path = "summary_tests.rs"]
mod tests;
