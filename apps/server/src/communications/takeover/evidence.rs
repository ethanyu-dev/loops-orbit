use crate::{AppState, error::ApiResult, knowledge};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// 总证据有界，避免整个个人资料库直接进入外发回答上下文。
const MAX_EVIDENCE: usize = 16;
const MAX_ANSWER_CHARS: usize = 1500;

/// 文本与原始版本一同保存，发送前重新核对删除、暂停和修正。
#[derive(Serialize)]
pub(super) struct Evidence {
    /// 提示词内短标识，不能作为对外消息引用。
    pub id: String,
    /// 本人批准的对外知识正文，不包含原始私聊或私人证据。
    pub text: String,
    /// 已发布知识及其确认版本，只供发送前核验。
    #[serde(skip)]
    origin: (Uuid, i64),
}
/// 对外起草只检索本人已确认的知识正文，不读取个人记忆、私聊原文和来源证据。
pub(super) async fn gather(
    state: &AppState,
    question: &str,
    topic: &str,
) -> ApiResult<Vec<Evidence>> {
    let query = format!(
        "{topic} {}",
        question.chars().take(1000).collect::<String>()
    );
    Ok(knowledge::search(state, &query)
        .await?
        .into_iter()
        .enumerate()
        .map(|(index, entry)| Evidence {
            id: format!("e{index}"),
            text: entry.content,
            origin: (entry.id, entry.version),
        })
        .collect())
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
/// 调用方持有沟通锁，已撤回、已编辑或删除的知识一律停止外发。
pub(super) async fn current(state: &AppState, evidence: &[Evidence]) -> ApiResult<bool> {
    for item in evidence {
        if !knowledge::current(state, item.origin.0, item.origin.1).await? {
            return Ok(false);
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
            origin: (Uuid::nil(), 1),
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
