use crate::{
    AppState, auth,
    communications::{self, Document, store},
    error::ApiResult,
    memory::lexical,
};
use serde_json::json;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

// 原文按字符切块，保留小段重叠以减少跨块语义丢失；网络故障按行退避。
const CHUNK_CHARS: usize = 1800;
const OVERLAP_CHARS: usize = 160;
const EMBEDDING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// 切块保持 UTF-8 完整；空文本不产生无意义的向量。
fn chunks(text: &str) -> Vec<String> {
    let chars: Vec<_> = text.chars().collect();
    let mut output = vec![];
    let mut offset = 0;
    while offset < chars.len() {
        let end = (offset + CHUNK_CHARS).min(chars.len());
        output.push(chars[offset..end].iter().collect());
        if end == chars.len() {
            break;
        }
        offset = end - OVERLAP_CHARS;
    }
    output
}

/// 发布与词法分块在同一事务中提交，首次回答无需等待 embedding；编辑删除旧向量。
pub(crate) async fn published(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> ApiResult<()> {
    sqlx::query("DELETE FROM rag_chunks WHERE knowledge_id=$1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    let entry:Option<(String,String,Vec<String>,i64)>=sqlx::query_as("SELECT title,content,tags,version FROM knowledge_entries WHERE id=$1 AND status='published'").bind(id).fetch_optional(&mut **tx).await?;
    if let Some((title, content, tags, version)) = entry {
        let terms = lexical::tokens(&format!("{title} {} {content}", tags.join(" "))).join(" ");
        sqlx::query("INSERT INTO rag_chunks(id,scope,knowledge_id,revision,source_hash,ordinal,title,content,terms) VALUES($1,'published',$2,$3,$4,0,$5,$6,$7)")
            .bind(Uuid::new_v4()).bind(id).bind(version).bind(auth::hash(&content)).bind(title).bind(content).bind(terms).execute(&mut **tx).await?;
    }
    Ok(())
}

/// 原文快照独立于文件生命周期；相同文件版本重放复用快照，不重复占用空间。
pub(crate) async fn snapshot(
    state: &AppState,
    doc: &Document,
    messages: &[store::Message],
) -> ApiResult<(Uuid, String)> {
    let label: String = sqlx::query_scalar("SELECT label FROM communication_sources WHERE id=$1")
        .bind(doc.source_id)
        .fetch_one(&state.pool)
        .await?;
    let id:Uuid=sqlx::query_scalar("INSERT INTO knowledge_snapshots(id,origin_id,origin_version,raw_hash,source_label,source_day,messages) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(origin_id,origin_version,raw_hash) DO UPDATE SET origin_id=excluded.origin_id RETURNING id")
        .bind(Uuid::new_v4()).bind(doc.id).bind(doc.version).bind(&doc.raw_hash).bind(&label).bind(&doc.day).bind(json!(messages)).fetch_one(&state.pool).await?;
    Ok((id, label))
}

/// 每步修复一份已发布知识或索引一份私有文件；问答路径不负责遍历和建库。
pub async fn documents_step(state: &AppState) -> ApiResult<bool> {
    let _guard = state.communications.lock().await;
    let missing:Option<Uuid>=sqlx::query_scalar("SELECT k.id FROM knowledge_entries k WHERE k.status='published' AND NOT EXISTS(SELECT 1 FROM rag_chunks r WHERE r.knowledge_id=k.id AND r.revision=k.version) ORDER BY k.updated_at LIMIT 1").fetch_optional(&state.pool).await?;
    if let Some(id) = missing {
        let mut tx = state.pool.begin().await?;
        published(&mut tx, id).await?;
        tx.commit().await?;
        return Ok(true);
    }
    if state.config.communications.is_none() {
        return Ok(false);
    }
    let doc:Option<Document>=sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {} FROM communication_documents d WHERE extraction_version=1 AND source_id IN(SELECT id FROM communication_sources WHERE enabled AND owner='admin' AND NOT removal_pending) AND NOT EXISTS(SELECT 1 FROM rag_documents r WHERE r.document_id=d.id AND r.revision=d.version AND r.source_hash=d.raw_hash||':'||COALESCE(d.summary_hash,'')) AND NOT EXISTS(SELECT 1 FROM rag_index_retry x WHERE x.document_id=d.id AND x.retry_at>now()) ORDER BY day DESC,id LIMIT 1",communications::DOCUMENT_COLUMNS)))
        .fetch_optional(&state.pool).await?;
    let Some(doc) = doc else { return Ok(false) };
    let raw = match store::raw(state, &doc) {
        Ok(raw) => raw,
        Err(error) => {
            sqlx::query("INSERT INTO rag_index_retry(document_id,retry_at) VALUES($1,now()+interval '60 seconds') ON CONFLICT(document_id) DO UPDATE SET retry_at=excluded.retry_at")
                .bind(doc.id).execute(&state.pool).await?;
            return Err(error);
        }
    };
    let (snapshot_id, label) = snapshot(state, &doc, &raw).await?;
    let hash = format!(
        "{}:{}",
        doc.raw_hash,
        doc.summary_hash.as_deref().unwrap_or_default()
    );
    let summary = store::summary(state, &doc).ok();
    drop(_guard);
    let chunk_day = doc.day.clone();
    let chunk_label = label.clone();
    // 分词在阻塞线程执行且不持有沟通锁，避免大文件阻塞实时提问。
    let pieces = tokio::task::spawn_blocking(move || {
        let mut pieces = vec![];
        for message in raw.iter().filter(|m| !m.deleted && !m.text.trim().is_empty()) {
            for text in chunks(&message.text) {
                pieces.push((text, json!({
                    "kind":"original", "snapshot_id":snapshot_id, "day":chunk_day,
                    "sender":message.display_name(), "is_me":message.is_me, "time":message.create_time
                })));
            }
        }
        if let Some(summary) = summary {
            for item in summary.items {
                for text in chunks(&format!("{}\n原文：{}", item.text, item.quote)) {
                    pieces.push((text, json!({
                        "kind":"summary", "day":chunk_day, "is_me":item.is_me, "time":item.create_time
                    })));
                }
            }
        }
        pieces.into_iter().enumerate().map(|(ordinal, (content, payload))| {
            let terms = lexical::tokens(&format!("{chunk_label} {chunk_day} {content}")).join(" ");
            json!({
                "id":Uuid::new_v4(), "ordinal":ordinal, "content":content,
                "payload":payload, "terms":terms
            })
        }).collect::<Vec<_>>()
    }).await.map_err(super::unavailable)?;
    let _guard = state.communications.lock().await;
    let current:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM communication_documents WHERE id=$1 AND version=$2 AND raw_hash||':'||COALESCE(summary_hash,'')=$3)")
        .bind(doc.id).bind(doc.version).bind(&hash).fetch_one(&state.pool).await?;
    if !current {
        return Ok(true);
    }
    let mut tx = state.pool.begin().await?;
    sqlx::query("DELETE FROM rag_chunks WHERE document_id=$1")
        .bind(doc.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO rag_chunks(id,scope,document_id,revision,source_hash,ordinal,title,content,payload,terms) SELECT x.id,'private',$1,$2,$3,x.ordinal,$4,x.content,x.payload,x.terms FROM jsonb_to_recordset($5) AS x(id UUID,ordinal INTEGER,content TEXT,payload JSONB,terms TEXT)")
        .bind(doc.id).bind(doc.version).bind(&hash).bind(&label).bind(json!(pieces)).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO rag_documents(document_id,revision,source_hash) VALUES($1,$2,$3) ON CONFLICT(document_id) DO UPDATE SET revision=excluded.revision,source_hash=excluded.source_hash")
        .bind(doc.id).bind(doc.version).bind(hash).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM rag_index_retry WHERE document_id=$1")
        .bind(doc.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

/// embedding 在锁外计算，每行独立退避，坏行不会阻塞其余知识。版本切换自动补建。
pub async fn vectors_step(state: &AppState) -> ApiResult<bool> {
    let Some(config) = state
        .config
        .memory
        .as_ref()
        .and_then(|m| m.embedding.as_ref())
    else {
        return Ok(false);
    };
    let version = config.version();
    let row:Option<(Uuid,String,String)>=sqlx::query_as("SELECT id,title,content FROM rag_eligible WHERE embedding_version IS DISTINCT FROM $1 AND embedding_retry_at<=now() ORDER BY (scope='published') DESC,embedding_retry_at,id LIMIT 1")
        .bind(&version).fetch_optional(&state.pool).await?;
    let Some((id, title, content)) = row else {
        return Ok(false);
    };
    let result = tokio::time::timeout(
        EMBEDDING_TIMEOUT,
        config.embed(&state.http, &format!("{title}\n{content}")),
    )
    .await;
    match result {
        Ok(Ok(vector)) => {
            sqlx::query("UPDATE rag_chunks SET embedding=$2::public.vector,embedding_version=$3,embedding_failures=0 WHERE id=$1")
                .bind(id).bind(vector).bind(version).execute(&state.pool).await?;
        }
        _ => {
            sqlx::query("UPDATE rag_chunks SET embedding_failures=embedding_failures+1,embedding_retry_at=now()+make_interval(secs=>LEAST(3600,60*power(2,LEAST(embedding_failures,6)))::double precision) WHERE id=$1")
                .bind(id).execute(&state.pool).await?;
            tracing::warn!("RAG 向量生成失败，已延迟重试，词法检索仍可使用");
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    // 只验证字符边界及重叠；不覆盖模型语义召回效果。
    #[test]
    fn unicode_chunks_are_bounded_and_overlap() {
        let input = "中文🙂".repeat(1400);
        let parts = chunks(&input);
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| p.chars().count() <= CHUNK_CHARS));
        assert_eq!(
            parts[0]
                .chars()
                .skip(CHUNK_CHARS - OVERLAP_CHARS)
                .collect::<String>(),
            parts[1].chars().take(OVERLAP_CHARS).collect::<String>()
        );
        assert!(chunks("").is_empty());
    }
}
