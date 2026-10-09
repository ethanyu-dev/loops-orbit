use super::{DOCUMENT_COLUMNS, Document, images, search, store};
use crate::{AppState, error::ApiResult};
use serde_json::Value;
use uuid::Uuid;

// 每轮最多检查十份日资料，逐份释放采集锁；不会在管理请求中读取正文。
const BATCH_SIZE: i64 = 10;
// 已检查的文件定期复核，发现本地文件变更及图片识别失败状态的变化。
const RECHECK_SECONDS: f64 = 60.0;

/// 索引模型变化也使进度失效，不能继续显示旧模型下的“可用”。
fn embedding_version(state: &AppState) -> String {
    state
        .config
        .memory
        .as_ref()
        .and_then(|m| m.embedding.as_ref())
        .map(|config| config.version())
        .unwrap_or_default()
}

/// 只读取数据库聚合，不拿采集锁、不打开原文；版本不匹配的快照显示为“统计更新中”。
pub(super) async fn read(state: &AppState) -> ApiResult<Vec<Value>> {
    Ok(sqlx::query_scalar(
        "SELECT jsonb_build_object('source_id',d.source_id,'total',count(*),
        'checking',count(*) FILTER(WHERE p.document_id IS NULL),
        'ready',count(*) FILTER(WHERE p.status='ready'),
        'partial',count(*) FILTER(WHERE p.status='partial'),
        'summarizing',count(*) FILTER(WHERE p.status='summarizing'),
        'indexing',count(*) FILTER(WHERE p.status='indexing'),
        'errors',count(*) FILTER(WHERE p.status='errors'),
        'images',COALESCE(sum(p.images),0),'images_ready',COALESCE(sum(p.images_ready),0),
        'images_failed',COALESCE(sum(p.images_failed),0),'checked_at',min(p.checked_at))
        FROM communication_documents d LEFT JOIN communication_document_progress p
        ON p.document_id=d.id AND p.version=d.version AND d.extraction_version=1
        AND p.summary_hash IS NOT DISTINCT FROM d.summary_hash
        AND p.summary_error IS NOT DISTINCT FROM d.summary_error AND p.embedding_version=$1
        GROUP BY d.source_id ORDER BY d.source_id",
    )
    .bind(embedding_version(state))
    .fetch_all(&state.pool)
    .await?)
}

/// 向量刚完成时立即排队复核，不必等周期扫描；正文版本变化由读侧自动排除旧快照。
pub(super) async fn invalidate(state: &AppState, id: Uuid) -> ApiResult<()> {
    sqlx::query("DELETE FROM communication_document_progress WHERE document_id=$1")
        .bind(id)
        .execute(&state.pool)
        .await?;
    Ok(())
}

/// 有界后台单步供 worker 和数据库夹具共用；旧数据逐批建立快照，不阻塞迁移及首屏。
pub async fn step(state: &AppState) -> ApiResult<()> {
    let version = embedding_version(state);
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT d.id FROM communication_documents d LEFT JOIN communication_document_progress p ON p.document_id=d.id
        WHERE p.document_id IS NULL OR p.version<>d.version
        OR p.summary_hash IS DISTINCT FROM d.summary_hash OR p.summary_error IS DISTINCT FROM d.summary_error
        OR p.embedding_version<>$1 OR p.checked_at<now()-make_interval(secs=>$2)
        ORDER BY p.checked_at NULLS FIRST,d.id LIMIT $3"
    ).bind(&version).bind(RECHECK_SECONDS).bind(BATCH_SIZE).fetch_all(&state.pool).await?;
    for id in ids {
        // 等待正在提交的单页完成；锁仅覆盖一份文档，不跨完整工作空间持有。
        let _guard = state.communications.lock().await;
        let doc: Option<Document> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {DOCUMENT_COLUMNS} FROM communication_documents WHERE id=$1"
        )))
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
        let Some(doc) = doc else { continue };
        let (status, pictures, ready, failed) = inspect(state, &doc).await?;
        sqlx::query("INSERT INTO communication_document_progress(document_id,version,summary_hash,summary_error,embedding_version,status,images,images_ready,images_failed)
            VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT(document_id) DO UPDATE SET
            version=excluded.version,summary_hash=excluded.summary_hash,summary_error=excluded.summary_error,
            embedding_version=excluded.embedding_version,status=excluded.status,images=excluded.images,
            images_ready=excluded.images_ready,images_failed=excluded.images_failed,checked_at=now()")
            .bind(doc.id).bind(doc.version).bind(&doc.summary_hash).bind(&doc.summary_error)
            .bind(&version).bind(status).bind(pictures).bind(ready).bind(failed).execute(&state.pool).await?;
    }
    Ok(())
}

/// 沿用原有文件哈希、证据与向量版本校验；失败文件单独计数，不让整页连接信息失败。
async fn inspect(state: &AppState, doc: &Document) -> ApiResult<(&'static str, i64, i64, i64)> {
    let raw = match store::raw(state, doc) {
        Ok(raw) => raw,
        Err(_) => return Ok(("errors", 0, 0, 0)),
    };
    let pictures = raw.iter().map(|m| images::keys(m).len() as i64).sum();
    let notes = images::notes(state, doc).await?;
    let ready = notes.iter().filter(|n| n.description.is_some()).count() as i64;
    let failed = notes.iter().filter(|n| n.error.is_some()).count() as i64;
    let status = if doc.summary_status == "partial" && store::summary(state, doc).is_ok() {
        "partial"
    } else if doc.summary_error.is_some() {
        "errors"
    } else if let Ok(summary) = store::summary(state, doc) {
        let entry = search::summary_entry(doc, &summary);
        let indexed = if entry.content.is_empty() {
            true
        } else if let Some(config) = state
            .config
            .memory
            .as_ref()
            .and_then(|m| m.embedding.as_ref())
        {
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memory_vectors WHERE owner=$1 AND id=$2 AND content_hash=$3 AND version=$4)")
                .bind(search::VECTOR_OWNER).bind(doc.id).bind(entry.hash()).bind(config.version()).fetch_one(&state.pool).await?
        } else {
            true
        };
        if indexed { "ready" } else { "indexing" }
    } else {
        "summarizing"
    };
    Ok((status, pictures, ready, failed))
}
