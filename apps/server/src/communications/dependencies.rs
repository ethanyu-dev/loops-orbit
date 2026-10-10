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
    cancel_scope(state, document, source.map(|id| vec![id.to_string()])).await
}
/// 整批来源只锁定会话并扫描依赖一次，避免会话数量乘上数据库往返次数。
pub(crate) async fn cancel_sources(state: &AppState, sources: &[Uuid]) -> ApiResult<()> {
    cancel_scope(
        state,
        None,
        Some(sources.iter().map(Uuid::to_string).collect()),
    )
    .await
}
/// 单文档、单来源和批量来源共用同一取消边界。
async fn cancel_scope(
    state: &AppState,
    document: Option<Uuid>,
    sources: Option<Vec<String>>,
) -> ApiResult<()> {
    let mut tx = state.pool.begin().await?;
    // 日资料的正常追加也会取消旧摘要依赖，但不能清掉仍在进行的代答轮次。
    if document.is_none() {
        // 来源遗忘或暂停同时清除代答保存的正文、草稿和上下文；只保留投递 ID 与状态供去重核对。
        let takeover_sources: Vec<Uuid> = sqlx::query_scalar("SELECT source_id FROM communication_takeover_sessions WHERE ($1::text IS NULL OR source_id IN(SELECT source_id FROM communication_documents WHERE id::text=$1)) AND ($2::text[] IS NULL OR source_id::text=ANY($2)) ORDER BY source_id FOR UPDATE")
        .bind(document.map(|id|id.to_string())).bind(&sources).fetch_all(&mut *tx).await?;
        sqlx::query("UPDATE communication_takeover_jobs SET status=CASE WHEN status IN ('queued','evaluating') THEN 'ignored' ELSE status END,reason=CASE WHEN status IN ('queued','evaluating') THEN 'scope_changed' ELSE reason END,message='{}',context=NULL,draft_answer=NULL,answer=NULL WHERE source_id=ANY($1)")
        .bind(&takeover_sources).execute(&mut *tx).await?;
        sqlx::query("UPDATE communication_takeover_turns SET status='closed',inputs='[]' WHERE source_id=ANY($1)")
        .bind(&takeover_sources).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM communication_takeover_inputs WHERE source_id=ANY($1)")
            .bind(&takeover_sources)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE communication_takeover_sessions SET epoch=$2,boundary_ms=(extract(epoch FROM clock_timestamp())*1000)::bigint,topic=NULL,version=version+1,updated_at=now() WHERE source_id=ANY($1)")
        .bind(&takeover_sources).bind(Uuid::new_v4()).execute(&mut *tx).await?;
    }
    sqlx::query("SELECT id FROM conversations WHERE owner='admin' OR owner IN(SELECT 'feishu:'||open_id FROM communication_connections) ORDER BY id FOR UPDATE").execute(&mut *tx).await?;
    let ids:Vec<Uuid>=sqlx::query_scalar("UPDATE followups SET status='cancelled',version=version+1,lease_token=NULL,error='communication_source_changed',updated_at=now() WHERE memory_versions ? '_communication' AND ($1::text IS NULL OR memory_versions->'_communication'->>'document_id'=$1) AND ($2::text[] IS NULL OR memory_versions->'_communication'->>'source_id'=ANY($2)) AND status IN('scheduled','checking','queued') RETURNING id")
        .bind(document.map(|id|id.to_string())).bind(&sources).fetch_all(&mut *tx).await?;
    sqlx::query("UPDATE outbox SET status='cancelled',lease_until=NULL WHERE followup_id=ANY($1) AND status IN('queued','running')")
        .bind(&ids).execute(&mut *tx).await?;
    // 历史提醒可保留用户确认过的事项，但失效资料的原话不继续滞留在内部依赖字段。
    sqlx::query("UPDATE followups SET memory_versions=jsonb_set(memory_versions,'{_communication,evidence}','null'::jsonb) WHERE memory_versions ? '_communication' AND ($1::text IS NULL OR memory_versions->'_communication'->>'document_id'=$1) AND ($2::text[] IS NULL OR memory_versions->'_communication'->>'source_id'=ANY($2))")
        .bind(document.map(|id|id.to_string())).bind(&sources).execute(&mut *tx).await?;
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
