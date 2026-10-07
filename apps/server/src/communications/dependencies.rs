use super::{DOCUMENT_COLUMNS, Document, Reference, search, store};
use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

/// 调用方在创建事务期间持有沟通写锁，来源不能在验证和绑定之间被删除。
pub(crate) async fn bind(state: &AppState, owner: &str, reference: &Reference) -> ApiResult<Value> {
    if !search::allowed(state, owner).await? {
        return Err(ApiError(StatusCode::FORBIDDEN, "communication_forbidden"));
    }
    let sql = format!(
        "SELECT {DOCUMENT_COLUMNS} FROM communication_documents WHERE extraction_version=1 AND id=$1 AND version=$2 AND source_id IN(SELECT id FROM communication_sources WHERE enabled)"
    );
    let doc: Document = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(reference.document_id)
        .bind(reference.version)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(ApiError(
            StatusCode::CONFLICT,
            "communication_source_changed",
        ))?;
    let summary = store::summary(state, &doc)?;
    let item = summary.items.get(reference.item).ok_or(ApiError(
        StatusCode::BAD_REQUEST,
        "communication_source_changed",
    ))?;
    Ok(
        json!({"document_id":doc.id,"source_id":doc.source_id,"version":doc.version,"raw_hash":doc.raw_hash,"summary_hash":doc.summary_hash,"item":reference.item,"evidence":item}),
    )
}
/// 发送前重新校验权限、版本与原文文件；遗忘或暂停不留下可发送的孤立提醒。
pub(crate) async fn valid(state: &AppState, owner: &str, value: &Value) -> ApiResult<bool> {
    if value.is_null() {
        return Ok(true);
    }
    let Some(id) = value["document_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
    else {
        return Ok(false);
    };
    let reference = Reference {
        document_id: id,
        version: value["version"].as_i64().unwrap_or(-1),
        item: value["item"].as_u64().unwrap_or(u64::MAX) as usize,
    };
    match bind(state, owner, &reference).await {
        Ok(current) => Ok(current == *value),
        Err(error) if !error.0.is_server_error() => Ok(false),
        Err(error) => Err(error),
    }
}
/// 先停止依赖任务再改文件；即使后续磁盘失败，也不能基于旧证据继续投递。
pub(crate) async fn cancel(
    state: &AppState,
    document: Option<Uuid>,
    source: Option<Uuid>,
) -> ApiResult<()> {
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT id FROM conversations WHERE owner='admin' OR owner IN(SELECT 'feishu:'||open_id FROM communication_connections) ORDER BY id FOR UPDATE").execute(&mut *tx).await?;
    let ids:Vec<Uuid>=sqlx::query_scalar("UPDATE followups SET status='cancelled',version=version+1,lease_token=NULL,error='communication_source_changed',updated_at=now() WHERE memory_versions ? '_communication' AND ($1::text IS NULL OR memory_versions->'_communication'->>'document_id'=$1) AND ($2::text IS NULL OR memory_versions->'_communication'->>'source_id'=$2) AND status IN('scheduled','checking','queued') RETURNING id")
        .bind(document.map(|id|id.to_string())).bind(source.map(|id|id.to_string())).fetch_all(&mut *tx).await?;
    for id in ids {
        sqlx::query(include_str!("../sql/followup_cancel_outbox.sql"))
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    // 历史提醒可保留用户确认过的事项，但失效资料的原话不继续滞留在内部依赖字段。
    sqlx::query("UPDATE followups SET memory_versions=jsonb_set(memory_versions,'{_communication,evidence}','null'::jsonb) WHERE memory_versions ? '_communication' AND ($1::text IS NULL OR memory_versions->'_communication'->>'document_id'=$1) AND ($2::text IS NULL OR memory_versions->'_communication'->>'source_id'=$2)")
        .bind(document.map(|id|id.to_string())).bind(source.map(|id|id.to_string())).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
/// 来源修正或遗忘后保守清空关联身份的历史推理上下文，保留用户可查看的聊天记录。
pub(crate) async fn invalidate_context(state: &AppState) -> ApiResult<()> {
    let Some(open_id): Option<String> =
        sqlx::query_scalar("SELECT open_id FROM communication_connections WHERE owner='admin'")
            .fetch_optional(&state.pool)
            .await?
    else {
        return Ok(());
    };
    let _guard = state.memory.lock().await;
    for owner in ["admin".to_owned(), format!("feishu:{open_id}")] {
        sqlx::query("INSERT INTO memory_owners(owner) VALUES($1) ON CONFLICT DO NOTHING")
            .bind(&owner)
            .execute(&state.pool)
            .await?;
        crate::memory::boundary::invalidate(state, &owner).await?;
    }
    Ok(())
}
