use super::{
    DOCUMENT_COLUMNS, Document, SOURCE_COLUMNS, Source, dependencies, search, store, unavailable,
};
use crate::{AppState, error::ApiResult};
use std::collections::BTreeMap;
use uuid::Uuid;

/// 统一按北京时间分日；原始毫秒时间不变，避免浏览器时区改变归档边界。
pub(super) fn day(timestamp: i64) -> ApiResult<String> {
    Ok(chrono::DateTime::from_timestamp_millis(timestamp)
        .ok_or_else(|| unavailable("无效消息时间"))?
        .with_timezone(&super::LOCAL_TIMEZONE)
        .format("%Y-%m-%d")
        .to_string())
}
/// 旧 UTC 快照先完整读写再事务切换，失败时原指针仍有效；成功后回收旧文件。
pub(super) async fn migrate(state: &AppState) -> ApiResult<()> {
    let _guard = state.communications.lock().await;
    let source: Option<Source> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {SOURCE_COLUMNS} FROM communication_sources WHERE day_timezone='UTC' AND NOT removal_pending LIMIT 1"
    )))
    .fetch_optional(&state.pool)
    .await?;
    let Some(source) = source else { return Ok(()) };
    let old: Vec<Document> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {DOCUMENT_COLUMNS} FROM communication_documents WHERE source_id=$1"
    )))
    .bind(source.id)
    .fetch_all(&state.pool)
    .await?;
    let mut groups: BTreeMap<String, BTreeMap<String, store::Message>> = BTreeMap::new();
    for doc in &old {
        for message in store::raw_unchecked(state, doc)? {
            let messages = groups.entry(day(message.create_time)?).or_default();
            if messages
                .get(&message.message_id)
                .is_none_or(|m| m.update_time <= message.update_time)
            {
                messages.insert(message.message_id.clone(), message);
            }
        }
    }
    let mut current = vec![];
    for (day, messages) in groups {
        let previous = old.iter().find(|d| d.day == day);
        let mut doc = Document {
            id: previous.map_or_else(Uuid::new_v4, |d| d.id),
            source_id: source.id,
            day,
            raw_hash: String::new(),
            version: previous.map_or(1, |d| d.version + 1),
            extraction_version: if old.iter().all(|d| d.extraction_version == 1) {
                1
            } else {
                0
            },
            summary_hash: None,
            summary_error: None,
            summary_status: "pending".into(),
            summary_attempts: 0,
        };
        let mut messages: Vec<_> = messages.into_values().collect();
        messages
            .sort_by(|a, b| (a.create_time, &a.message_id).cmp(&(b.create_time, &b.message_id)));
        if let Some(previous) = previous
            && store::raw_unchecked(state, previous)? == messages
        {
            doc = previous.clone();
        } else {
            doc.raw_hash = store::write_raw(state, &doc, &messages)?;
        }
        current.push(doc);
    }
    let changed: Vec<_> = old
        .iter()
        .filter(|doc| {
            !current
                .iter()
                .any(|d| d.id == doc.id && d.raw_hash == doc.raw_hash)
        })
        .collect();
    if !changed.is_empty() {
        for doc in &changed {
            dependencies::cancel(state, Some(doc.id), None).await?;
        }
        // 未核对资料已在启动时隔离；逐来源迁移不能反复推进新聊天的遗忘边界。
        if changed.iter().any(|doc| doc.extraction_version == 1) {
            dependencies::invalidate_context(state).await?;
        }
    }
    let mut tx = state.pool.begin().await?;
    for doc in &changed {
        sqlx::query("DELETE FROM memory_vectors WHERE owner='communications:admin' AND id=$1")
            .bind(doc.id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("DELETE FROM communication_documents WHERE source_id=$1")
        .bind(source.id)
        .execute(&mut *tx)
        .await?;
    for doc in &current {
        sqlx::query("INSERT INTO communication_documents(id,source_id,day,raw_hash,version,summary_hash,summary_error,extraction_version,summary_status,summary_attempts) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)").bind(doc.id).bind(doc.source_id).bind(&doc.day).bind(&doc.raw_hash).bind(doc.version).bind(&doc.summary_hash).bind(&doc.summary_error).bind(doc.extraction_version).bind(&doc.summary_status).bind(doc.summary_attempts).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE communication_sources SET day_timezone='Asia/Shanghai',version=version+1 WHERE id=$1").bind(source.id).execute(&mut *tx).await?;
    tx.commit().await?;
    for doc in &current {
        store::collect(state, doc)?;
    }
    for doc in &old {
        if !current.iter().any(|d| d.id == doc.id) {
            search::remove_vector(state, doc.id).await?;
            let directory = store::directory(state, source.id)?.join(doc.id.to_string());
            if directory.exists() {
                std::fs::remove_dir_all(directory).map_err(unavailable)?;
            }
        }
    }
    Ok(())
}
