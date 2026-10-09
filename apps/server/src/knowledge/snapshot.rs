use super::worker::CHUNK_BYTES;
use crate::{
    AppState,
    communications::{Document, store},
    error::ApiResult,
};
use serde_json::{Value, json};

/// 仅在读取期间使用文件引用，调用方持有沟通锁；返回值完全独立于文件生命周期。
/// 超预算消息明确计数，机器人和已撤回内容不作为知识输入。
pub(super) async fn from_documents(state: &AppState, docs: &[Document]) -> ApiResult<(Value, i64)> {
    let mut messages = vec![];
    let mut skipped = 0i64;
    for (conversation, doc) in docs.iter().enumerate() {
        let raw = store::raw(state, doc)?;
        let (snapshot_id, label) = crate::rag::index::snapshot(state, doc, &raw).await?;
        for message in raw {
            if message.deleted
                || message.sender_type != "user"
                || message.text.trim().is_empty()
                || message.text.trim_start().starts_with("[agent]")
            {
                continue;
            }
            let value = json!({"message_id":format!("m{}",messages.len()),"conversation":conversation,"text":message.text,"is_me":message.is_me,"snapshot_id":snapshot_id,"source_label":label,"source_day":doc.day});
            if value.to_string().len() > CHUNK_BYTES {
                skipped += 1;
                continue;
            }
            messages.push(value);
        }
    }
    Ok((json!(messages), skipped))
}
