use super::{DOCUMENT_COLUMNS, Document, store, summary::Summary, unavailable};
use crate::{
    AppState, auth,
    error::ApiResult,
    memory::{Entry, embedding, lexical::Lexical},
};
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

// 独立索引命名空间，不与管理员个人记忆或任何访客混用。
pub(super) const VECTOR_OWNER: &str = "communications:admin";
const MAX_RESULTS: usize = 6;
const CONTEXT_PROMPT: &str = include_str!("../../prompts/communication_context.md");
/// 检索结果携带原文证据，而不是只返回无来源的自然语言总结。
#[derive(Serialize)]
pub struct Hit {
    /// 原文及摘要版本。
    pub document: Document,
    /// 所选会话展示名。
    pub label: String,
    /// 匹配的原始文本消息；向量召回时保留摘要引文。
    pub messages: Vec<store::Message>,
    /// 可选的经过证据校验的机器整理。
    pub summary: Option<Summary>,
}
/// 管理员和已授权的同一个飞书账号可检索；其他机器人访客不得跨身份召回。
pub(crate) async fn allowed(state: &AppState, owner: &str) -> ApiResult<bool> {
    if state.config.communications.is_none() {
        return Ok(false);
    }
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM communication_connections WHERE owner=$1 OR 'feishu:'||open_id=$1)").bind(owner).fetch_one(&state.pool).await?)
}
/// 文件读取前先限制来源，缺失或被编辑的文件不退回数据库旧正文。
pub(crate) async fn documents(state: &AppState) -> ApiResult<Vec<Document>> {
    let sql = format!(
        "SELECT {DOCUMENT_COLUMNS} FROM communication_documents WHERE source_id IN(SELECT id FROM communication_sources WHERE enabled) ORDER BY day DESC,id"
    );
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .fetch_all(&state.pool)
        .await?)
}
/// 复用词法与向量实现时只使用 Entry 作为索引适配器，不写入个人事实目录。
fn entry(id: Uuid, key: String, content: String) -> Entry {
    Entry {
        id,
        kind: "episode".into(),
        key,
        content,
        updated_at: chrono::DateTime::UNIX_EPOCH,
        expires_at: None,
        source_run: None,
        source_seq: 0,
        deleted: false,
    }
}
/// 文档版本纳入索引指纹，摘要更新后旧向量不能重新关联到新原文。
pub(super) fn summary_entry(doc: &Document, summary: &Summary) -> Entry {
    entry(
        doc.id,
        format!("{} {}", doc.day, doc.raw_hash),
        summary
            .items
            .iter()
            .map(|item| {
                format!(
                    "{} {} {} {}",
                    item.kind, item.sender_id, item.text, item.quote
                )
            })
            .chain(
                summary
                    .image_notes
                    .iter()
                    .filter_map(|n| n.description.clone()),
            )
            .collect::<Vec<_>>()
            .join("\n")
            .chars()
            .take(8000)
            .collect(),
    )
}
/// BM25 针对逐条消息，向量针对有出处的摘要，按 RRF 融合成可查看的日文档。
pub async fn search(state: &AppState, owner: &str, query: &str) -> ApiResult<Vec<Hit>> {
    if query.trim().is_empty() || !allowed(state, owner).await? {
        return Ok(vec![]);
    }
    let (mut entries, mut lookup, mut summaries, mut hits) =
        (vec![], HashMap::new(), HashMap::new(), HashMap::new());
    {
        let _guard = state.communications.lock().await;
        for doc in documents(state).await? {
            let Ok(raw) = store::raw(state, &doc) else {
                continue;
            };
            let label: String =
                sqlx::query_scalar("SELECT label FROM communication_sources WHERE id=$1")
                    .bind(doc.source_id)
                    .fetch_one(&state.pool)
                    .await?;
            let summary = store::summary(state, &doc).ok();
            if let Some(summary) = &summary {
                summaries.insert(doc.id, summary_entry(&doc, summary));
            }
            let notes = super::images::notes(state, &doc).await?;
            for message in raw.into_iter().filter(|m| !m.deleted) {
                let visual = notes
                    .iter()
                    .filter(|n| n.message_id == message.message_id)
                    .filter_map(|n| n.description.as_deref())
                    .collect::<Vec<_>>()
                    .join("\n");
                if message.text.trim().is_empty() && visual.is_empty() {
                    continue;
                }
                let digest = hex::decode(auth::hash(&format!("{}:{}", doc.id, message.message_id)))
                    .expect("十六进制摘要");
                let id = Uuid::from_bytes(digest[..16].try_into().expect("固定摘要长度"));
                entries.push(entry(
                    id,
                    label.clone(),
                    format!(
                        "{} {} {} {}",
                        message.display_name(),
                        message.create_time,
                        message.text,
                        visual
                    ),
                ));
                lookup.insert(id, (doc.id, message));
            }
            hits.insert(
                doc.id,
                Hit {
                    document: doc,
                    label,
                    messages: vec![],
                    summary,
                },
            );
        }
    }
    let query = query.chars().take(2000).collect::<String>();
    let lexical_query = query.clone();
    let lexical = tokio::task::spawn_blocking(move || {
        Lexical::build(&entries, String::new())?.search(&lexical_query)
    })
    .await
    .map_err(unavailable)?
    .map_err(unavailable)?;
    let mut scores: BTreeMap<Uuid, f64> = BTreeMap::new();
    for (rank, id) in lexical.iter().enumerate() {
        if let Some((doc, message)) = lookup.get(id) {
            let score = scores.entry(*doc).or_default();
            // 一篇长文最多计一次词法名次，避免仅靠消息多获得高分。
            if *score == 0.0 {
                *score = 1.0 / (60.0 + rank as f64);
            }
            if let Some(hit) = hits.get_mut(doc)
                && hit.messages.len() < 3
            {
                hit.messages.push(message.clone());
            }
        }
    }
    if let Some(config) = state
        .config
        .memory
        .as_ref()
        .and_then(|m| m.embedding.as_ref())
    {
        let vectors = async {
            let vector = config.embed(&state.http, &query).await?;
            embedding::search(&state.pool, config, VECTOR_OWNER, &vector).await
        };
        if let Ok(Ok(vector)) =
            tokio::time::timeout(std::time::Duration::from_secs(3), vectors).await
        {
            for (rank, (id, hash)) in vector.iter().enumerate() {
                if summaries.get(id).is_some_and(|entry| entry.hash() == *hash) {
                    *scores.entry(*id).or_default() += 1.0 / (60.0 + rank as f64);
                }
            }
        }
    }
    let mut scores: Vec<_> = scores.into_iter().collect();
    scores.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let _guard = state.communications.lock().await;
    // 网络检索期间可能暂停或遗忘，返回前再次检查当前版本及文件。
    let current = documents(state).await?;
    if !allowed(state, owner).await? {
        return Ok(vec![]);
    }
    Ok(scores
        .into_iter()
        .filter_map(|(id, _)| hits.remove(&id))
        .filter(|hit| {
            current
                .iter()
                .any(|doc| doc.id == hit.document.id && doc.version == hit.document.version)
                && store::raw(state, &hit.document).is_ok()
        })
        .take(MAX_RESULTS)
        .collect())
}
/// 后台每步构建一篇摘要向量；故障时原文及 BM25 始终可用。
pub(super) async fn index_step(state: &AppState) -> ApiResult<()> {
    let Some(config) = state
        .config
        .memory
        .as_ref()
        .and_then(|m| m.embedding.as_ref())
    else {
        return Ok(());
    };
    for doc in documents(state).await? {
        let summary = {
            let _guard = state.communications.lock().await;
            store::summary(state, &doc).ok()
        };
        let Some(summary) = summary else { continue };
        let entry = summary_entry(&doc, &summary);
        if entry.content.is_empty() {
            continue;
        }
        let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memory_vectors WHERE owner=$1 AND id=$2 AND content_hash=$3 AND version=$4)").bind(VECTOR_OWNER).bind(doc.id).bind(entry.hash()).bind(config.version()).fetch_one(&state.pool).await?;
        if exists {
            continue;
        }
        let vector = config
            .embed(&state.http, &entry.content)
            .await
            .map_err(unavailable)?;
        let _guard = state.communications.lock().await;
        if documents(state).await?.iter().any(|current| {
            current.id == doc.id
                && current.version == doc.version
                && current.summary_hash == doc.summary_hash
        }) {
            embedding::save(&state.pool, config, VECTOR_OWNER, &entry, &vector)
                .await
                .map_err(unavailable)?;
            super::progress::invalidate(state, doc.id).await?;
        }
        break;
    }
    Ok(())
}
/// 即使关闭向量配置，遗忘也清理曾经建立的派生索引。
pub(crate) async fn remove_vector(state: &AppState, id: Uuid) -> ApiResult<()> {
    embedding::remove(&state.pool, VECTOR_OWNER, id)
        .await
        .map_err(unavailable)
}

/// 在对话中只附加带资料边界的检索证据，长度固定，绝不成为新的用户消息任务。
pub(crate) async fn context(
    state: &AppState,
    owner: &str,
    query: &str,
) -> ApiResult<Option<String>> {
    let hits = search(state, owner, query).await?;
    if hits.is_empty() {
        return Ok(None);
    }
    let _guard = state.communications.lock().await;
    let current = documents(state).await?;
    let mut evidence = vec![];
    let mut bytes = 0;
    for hit in hits {
        if !current
            .iter()
            .any(|d| d.id == hit.document.id && d.version == hit.document.version)
        {
            continue;
        }
        let raw = store::raw(state, &hit.document)?;
        let sender = |id: &str| {
            raw.iter()
                .find(|m| m.message_id == id)
                .map(|m| m.display_name())
                .unwrap_or("会话成员")
        };
        let time = |millis: i64| {
            chrono::DateTime::from_timestamp_millis(millis)
                .map(|t| t.with_timezone(&super::LOCAL_TIMEZONE).to_rfc3339())
                .unwrap_or_default()
        };
        let link = format!(
            "{}/communications?communication={}",
            state.config.public_url, hit.document.id
        );
        let notes = super::images::notes(state, &hit.document).await?;
        let value = json!({"document_id":hit.document.id,"version":hit.document.version,"source":hit.label,"day":hit.document.day,"source_url":link,
            "messages":hit.messages.iter().map(|m|json!({"sender":m.display_name(),"is_me":m.is_me,"time":time(m.create_time),"text":m.text.chars().take(1800).collect::<String>()})).collect::<Vec<_>>(),
            "summary":hit.summary.as_ref().map(|s|s.items.iter().enumerate().take(8).map(|(index,i)|json!({"item":index,"kind":i.kind,"text":i.text,"quote":i.quote,"sender":sender(&i.message_id),"is_me":i.is_me,"time":time(i.create_time)})).collect::<Vec<_>>()),
            "image_interpretations":notes.iter().filter_map(|n|n.description.as_ref().map(|text|json!({"sender":sender(&n.message_id),"interpretation":text,"source_url":link}))).take(8).collect::<Vec<_>>()});
        let size = value.to_string().len();
        if bytes + size > 16000 {
            break;
        }
        bytes += size;
        evidence.push(value);
    }
    Ok(Some(format!("{CONTEXT_PROMPT}\n{}", json!(evidence))))
}
