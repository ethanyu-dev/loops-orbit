use crate::{AppState, error::ApiResult};
use serde_json::{Value, json};
use uuid::Uuid;

// 限制总消息数与 UTF-8 字节数，采用比约八千 token 更保守、无需模型分词器的预算。
const MAX_MESSAGES: usize = 20;
const MAX_BYTES: usize = 8000;

/// 同一轮不可变的语义快照；匹配、起草和复核共享，不能各读一次可变历史。
pub(super) struct Context {
    /// 只包含当前输入与同片段已成功回答的历史，不含原始采集 payload。
    pub input: Value,
    /// 检索以当前批次为主，话题由匹配器补充。
    pub question: String,
    /// 上一条成功回复是否已经追问，防止连续澄清打扰。
    pub clarified: bool,
}
/// 限定同来源、同上下文片段；草稿、失败和不确定投递永远不进入历史。
pub(super) async fn snapshot(
    state: &AppState,
    turn: Uuid,
    revision: i64,
) -> ApiResult<Option<Context>> {
    let row: Option<(Uuid, Uuid, Value, Option<String>)> = sqlx::query_as("SELECT t.source_id,t.epoch,t.inputs,s.topic FROM communication_takeover_turns t JOIN communication_takeover_sessions s ON s.source_id=t.source_id AND s.epoch=t.epoch WHERE t.id=$1 AND t.revision=$2 AND t.status='pending'")
        .bind(turn).bind(revision).fetch_optional(&state.pool).await?;
    let Some((source, epoch, inputs, topic)) = row else {
        return Ok(None);
    };
    let inputs = inputs.as_array().cloned().unwrap_or_default();
    let current: Vec<Value> = inputs
        .iter()
        .map(|m| json!({"message_id":m["message_id"],"role":"user","text":m["text"]}))
        .collect();
    let question = inputs
        .iter()
        .filter_map(|m| m["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let mut budget = question.len();
    if current.is_empty() || current.len() > MAX_MESSAGES || budget > MAX_BYTES {
        return Ok(None);
    }
    let previous: Vec<(Value,String,Option<String>)> = sqlx::query_as("SELECT t.inputs,j.answer,j.reply_kind FROM communication_takeover_jobs j JOIN communication_takeover_turns t ON t.id=j.turn_id WHERE t.source_id=$1 AND t.epoch=$2 AND j.status='sent' AND j.topic=$3 ORDER BY j.updated_at DESC,j.id DESC LIMIT 10")
        .bind(source).bind(epoch).bind(&topic).fetch_all(&state.pool).await?;
    let clarified = previous
        .first()
        .is_some_and(|(_, _, kind)| kind.as_deref() == Some("clarify"));
    let mut history = vec![];
    for (inputs, answer, _) in previous {
        let mut exchange: Vec<Value> = inputs
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| json!({"role":"user","text":m["text"]}))
            .collect();
        exchange.push(json!({"role":"assistant","text":answer}));
        let size: usize = exchange
            .iter()
            .filter_map(|m| m["text"].as_str())
            .map(str::len)
            .sum();
        if history.len() + exchange.len() + current.len() > MAX_MESSAGES
            || budget + size > MAX_BYTES
        {
            break;
        }
        budget += size;
        exchange.append(&mut history);
        history = exchange;
    }
    Ok(Some(Context {
        input: json!({"incoming_message":question,"pending_messages":current,"history":history,"previous_topic":topic,"clarification_allowed":!clarified}),
        question,
        clarified,
    }))
}
/// 明确的整句取消或结束语可以直接静默；复杂语义仍由话题判断处理，不能靠包含关键词取消。
pub(super) fn closure(question: &str) -> Option<&'static str> {
    let last = question
        .lines()
        .rev()
        .find(|s| !s.trim().is_empty())?
        .trim()
        .trim_end_matches(['。', '！', '!', '，', ',', '~', '～']);
    match last {
        "算了" | "不用了" | "不需要了" | "取消" | "先不用了" | "不用回复了" => {
            Some("cancelled_by_sender")
        }
        "谢谢" | "谢谢了" | "好的谢谢" | "好的，谢谢" | "收到" | "明白了" | "了解了" | "好的"
        | "OK" | "ok" => Some("conversation_closed"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // 验证只匹配整句结束语，不误吞带“谢谢”的新问题；不替代模型的开放式中文意图判断。
    #[test]
    fn closure_only_matches_complete_last_utterance() {
        assert_eq!(closure("怎么申请？\n不用了。"), Some("cancelled_by_sender"));
        assert_eq!(closure("谢谢！"), Some("conversation_closed"));
        assert_eq!(closure("谢谢，那生产环境呢？"), None);
        assert_eq!(closure("取消权限要怎么申请？"), None);
    }
}
