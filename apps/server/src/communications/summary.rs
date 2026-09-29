use super::{
    DOCUMENT_COLUMNS, Document,
    store::{self, Message},
    unavailable,
};
use crate::{AppState, error::ApiResult};
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
        return Err(unavailable("无效整理条目"));
    }
    let source = messages
        .iter()
        .find(|m| m.message_id == item.message_id && !m.deleted && m.text.contains(&item.quote))
        .ok_or_else(|| unavailable("证据不存在"))?;
    if (item.kind == "my_commitment" && !source.is_me)
        || (item.kind == "their_commitment" && (source.is_me || source.sender_type != "user"))
    {
        return Err(unavailable("承诺归属错误"));
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
/// 每次只处理一份待整理日文件，失败退避，重启后继续。
pub(super) async fn step(state: &AppState) -> ApiResult<bool> {
    let sql = format!(
        "SELECT {DOCUMENT_COLUMNS} FROM communication_documents WHERE summary_hash IS NULL AND next_summary<=now() AND source_id IN(SELECT id FROM communication_sources WHERE enabled) ORDER BY next_summary LIMIT 1"
    );
    let Some(doc): Option<Document> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .fetch_optional(&state.pool)
        .await?
    else {
        return Ok(false);
    };
    sqlx::query(
        "UPDATE communication_documents SET next_summary=now()+interval '5 minutes' WHERE id=$1",
    )
    .bind(doc.id)
    .execute(&state.pool)
    .await?;
    let result = generate(state, &doc).await;
    if let Err(error) = result {
        sqlx::query("UPDATE communication_documents SET summary_error=$2 WHERE id=$1 AND version=$3 AND summary_hash IS NULL").bind(doc.id).bind(error.1).bind(doc.version).execute(&state.pool).await?;
    }
    Ok(true)
}
/// 模型在锁外计算，重新拿锁后比较原文版本；旧摘要永不覆盖更新或撤回。
async fn generate(state: &AppState, doc: &Document) -> ApiResult<()> {
    let raw = {
        let _guard = state.communications.lock().await;
        store::raw(state, doc)?
    };
    let mut chunks: Vec<Vec<&Message>> = vec![];
    let mut bytes = 0;
    let mut unsupported = 0;
    for message in &raw {
        if message.deleted {
            continue;
        }
        let size=serde_json::to_vec(&json!({"message_id":message.message_id,"sender_id":message.sender_id,"sender_name":message.display_name(),"sender_type":message.sender_type,"is_me":message.is_me,"create_time":message.create_time,"text":message.text})).map_err(unavailable)?.len();
        if message.text.trim().is_empty() || size > CHUNK_BYTES {
            unsupported += 1;
            continue;
        }
        if chunks.is_empty() || bytes + size > CHUNK_BYTES {
            chunks.push(vec![]);
            bytes = 0;
        }
        chunks.last_mut().expect("已有分块").push(message);
        bytes += size;
    }
    let mut items = vec![];
    for chunk in chunks {
        let input:Vec<Value>=chunk.iter().map(|m| json!({"message_id":m.message_id,"sender_id":m.sender_id,"sender_name":m.display_name(),"sender_type":m.sender_type,"is_me":m.is_me,"create_time":m.create_time,"text":m.text})).collect();
        let value = state
            .runtime
            .summarize_communications(&json!({"messages":input}))
            .await
            .map_err(unavailable)?;
        let mut candidates: Vec<Item> = serde_json::from_value(value).map_err(unavailable)?;
        if candidates.len() > MAX_ITEMS_PER_CHUNK {
            return Err(unavailable("条目过多"));
        }
        let allowed: Vec<Message> = chunk.into_iter().cloned().collect();
        for item in &mut candidates {
            let source = evidence(item, &allowed)?;
            item.sender_id = source.sender_id.clone();
            item.is_me = source.is_me;
            item.create_time = source.create_time;
        }
        items.extend(candidates);
    }
    let image_notes = {
        let _guard = state.communications.lock().await;
        super::images::notes(state, doc).await?
    };
    let summary = Summary {
        image_notes,
        raw_hash: doc.raw_hash.clone(),
        message_count: raw.len(),
        unsupported_count: unsupported,
        items,
    };
    let _guard = state.communications.lock().await;
    let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM communication_documents d JOIN communication_sources s ON s.id=d.source_id WHERE d.id=$1 AND d.version=$2 AND s.enabled)").bind(doc.id).bind(doc.version).fetch_one(&state.pool).await?;
    if !active {
        return Ok(());
    }
    let mut current = doc.clone();
    current.summary_hash = Some(store::write_summary(state, doc, &summary)?);
    sqlx::query("UPDATE communication_documents SET summary_hash=$2,summary_error=NULL WHERE id=$1 AND version=$3").bind(doc.id).bind(&current.summary_hash).bind(doc.version).execute(&state.pool).await?;
    store::collect(state, &current)?;
    Ok(())
}
