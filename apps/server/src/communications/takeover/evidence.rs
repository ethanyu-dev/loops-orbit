use super::super::{search, store};
use crate::{AppState, error::ApiResult, memory};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// 总证据有界，避免整个个人资料库直接进入外发回答上下文。
const MAX_EVIDENCE: usize = 16;
const MAX_TEXT_CHARS: usize = 2000;
const MAX_ANSWER_CHARS: usize = 1500;

/// 文本与原始版本一同保存，发送前重新核对删除、暂停和修正。
#[derive(Serialize)]
pub(super) struct Evidence {
    /// 提示词内短标识，不能作为对外消息引用。
    pub id: String,
    /// 检索到的原文；模型摘要不作为最终依据。
    pub text: String,
    /// 仅服务端使用的来源版本。
    #[serde(skip)]
    origin: Origin,
}
/// 两种现有知识来源各自使用原有的版本边界。
enum Origin {
    /// 个人记忆的稳定 ID 及完整指纹。
    Memory(Uuid, String),
    /// 沟通原文的文档 ID、版本、文件指纹和来源版本。
    Communication(Uuid, i64, String, i64),
}
/// 按问题和命中主题检索已有知识，排除输入本身与 agent 已发内容。
pub(super) async fn gather(
    state: &AppState,
    question: &str,
    topic: &str,
    incoming_id: &str,
) -> ApiResult<Vec<Evidence>> {
    let query = format!(
        "{topic} {}",
        question.chars().take(1000).collect::<String>()
    );
    let mut result = vec![];
    for entry in memory::search(state, "admin", &query)
        .await?
        .into_iter()
        .take(6)
    {
        result.push(Evidence {
            id: format!("e{}", result.len()),
            text: entry.content.chars().take(MAX_TEXT_CHARS).collect(),
            origin: Origin::Memory(entry.id, entry.hash()),
        });
    }
    for hit in search::search(state, "admin", &query).await? {
        let source_version: i64 =
            sqlx::query_scalar("SELECT version FROM communication_sources WHERE id=$1")
                .bind(hit.document.source_id)
                .fetch_one(&state.pool)
                .await?;
        for message in hit.messages {
            if result.len() >= MAX_EVIDENCE {
                break;
            }
            if message.message_id == incoming_id
                || message.deleted
                || message.text.trim().is_empty()
                || message.text.starts_with(super::AGENT_PREFIX)
            {
                continue;
            }
            result.push(Evidence {
                id: format!("e{}", result.len()),
                text: message.text.chars().take(MAX_TEXT_CHARS).collect(),
                origin: Origin::Communication(
                    hit.document.id,
                    hit.document.version,
                    hit.document.raw_hash.clone(),
                    source_version,
                ),
            });
        }
    }
    Ok(result)
}
/// 只接受结构化答案，空答案严格表示静默。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    /// 无法最终回答时是 null。
    answer: Option<String>,
    /// 关键结论的逐字出处。
    citations: Vec<Citation>,
}
/// 引文必须命中本次实际读取的知识。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Citation {
    /// 本次检索结果的标识。
    id: String,
    /// 原文逐字片段。
    quote: String,
}
/// 出处字符串匹配只能验证引用存在，语义支持另交 Jev 复核。
pub(super) fn validate(value: Value, evidence: &[Evidence]) -> Option<String> {
    let output: Answer = serde_json::from_value(value).ok()?;
    let answer = output.answer?.trim().to_owned();
    if answer.is_empty()
        || answer.chars().count() > MAX_ANSWER_CHARS
        || output.citations.is_empty()
        || output.citations.len() > MAX_EVIDENCE
    {
        return None;
    }
    if !output.citations.iter().all(|c| {
        c.quote.trim().chars().count() >= 4
            && evidence
                .iter()
                .any(|e| e.id == c.id && e.text.contains(&c.quote))
    }) {
        return None;
    }
    Some(answer)
}
/// 调用方持有资料和记忆锁，原文读取错误或版本变化一律停止外发。
pub(super) async fn current(state: &AppState, evidence: &[Evidence]) -> ApiResult<bool> {
    let config = state.config.memory.as_ref().expect("采集要求记忆配置");
    let entries =
        memory::store::read(&config.directory, "admin").map_err(super::super::unavailable)?;
    for item in evidence {
        match &item.origin {
            Origin::Memory(id, hash) => {
                if !entries
                    .iter()
                    .any(|e| e.id == *id && e.active() && e.hash() == *hash)
                {
                    return Ok(false);
                }
            }
            Origin::Communication(id, version, hash, source_version) => {
                let doc: Option<super::super::Document> = sqlx::query_as("SELECT d.id,d.source_id,d.day,d.raw_hash,d.version,d.extraction_version,d.summary_hash,d.summary_error,d.summary_status,d.summary_attempts FROM communication_documents d JOIN communication_sources s ON s.id=d.source_id WHERE d.id=$1 AND d.version=$2 AND d.raw_hash=$3 AND s.version=$4 AND s.enabled AND s.subscribed AND NOT s.removal_pending")
                    .bind(id).bind(version).bind(hash).bind(source_version).fetch_optional(&state.pool).await?;
                let Some(doc) = doc else { return Ok(false) };
                if store::raw_unchecked(state, &doc).is_err() {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    // 验证静默、伪造出处及长度边界；不证明真实模型能正确判断知识是否适合外发。
    #[test]
    fn only_accepts_answers_with_actual_quotes() {
        let evidence = vec![Evidence {
            id: "e0".into(),
            text: "请通过测试环境申请表提交用途。".into(),
            origin: Origin::Memory(Uuid::nil(), String::new()),
        }];
        assert!(validate(json!({"answer":null,"citations":[]}), &evidence).is_none());
        assert!(
            validate(
                json!({"answer":"已开通","citations":[{"id":"e0","quote":"权限已经开通"}]}),
                &evidence
            )
            .is_none()
        );
        assert!(validate(json!({"answer":"请提交测试环境申请表。","citations":[{"id":"e0","quote":"测试环境申请表"}]}), &evidence).is_some());
    }
}
