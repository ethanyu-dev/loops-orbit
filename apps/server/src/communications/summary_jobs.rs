use super::{DOCUMENT_COLUMNS, Document, configured, dependencies, search, store, summary};
use crate::{
    AppState,
    auth::Identity,
    error::{ApiError, ApiResult},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

// 同一文档版本最多执行三次；结构化证据错误在一次纠错后直接停止自动重试。
const MAX_ATTEMPTS: i32 = 3;
// 长日文件按心跳续租，避免固定总时限让大文档永远无法完成；崩溃后五分钟可回收。
const HEARTBEAT_SECONDS: u64 = 60;
const LEASE_SECONDS: f64 = 300.0;
// 临时供应商故障按执行次数退避；不会因页面刷新增加模型请求。
const RETRY_SECONDS: f64 = 300.0;

/// 有界单步供后台与集成测试共用；数据库原子领取避免重复执行。
pub async fn step(state: &AppState) -> ApiResult<bool> {
    // 最后一次领取后崩溃的任务也必须终止，不能永久显示运行中。
    sqlx::query("UPDATE communication_documents SET summary_status='failed',summary_error=COALESCE(summary_error,'communication_summary_interrupted') WHERE summary_status='running' AND next_summary<=now() AND summary_attempts>=$1")
        .bind(MAX_ATTEMPTS).execute(&state.pool).await?;
    let sql = format!(
        "UPDATE communication_documents SET summary_status='running',summary_attempts=summary_attempts+1,next_summary=now()+make_interval(secs=>$2) WHERE id=(SELECT id FROM communication_documents WHERE extraction_version=1 AND summary_status IN('pending','retry_wait','running') AND summary_attempts<$1 AND next_summary<=now() AND source_id IN(SELECT id FROM communication_sources WHERE enabled AND NOT removal_pending) ORDER BY next_summary LIMIT 1 FOR UPDATE SKIP LOCKED) RETURNING {DOCUMENT_COLUMNS}"
    );
    let Some(doc): Option<Document> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(MAX_ATTEMPTS)
        .bind(LEASE_SECONDS)
        .fetch_optional(&state.pool)
        .await?
    else {
        return Ok(false);
    };
    let generation = summary::generate(state, &doc);
    tokio::pin!(generation);
    let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECONDS));
    let outcome = loop {
        tokio::select! {
            result = &mut generation => match result {
                Ok(outcome) => break outcome,
                Err(error) => {
                    fail(state, &doc, error.1, error.1 == "storage_unavailable").await?;
                    return Ok(true);
                }
            },
            _ = heartbeat.tick() => {
                let renewed = sqlx::query("UPDATE communication_documents SET next_summary=now()+make_interval(secs=>$4) WHERE id=$1 AND version=$2 AND summary_attempts=$3 AND summary_status='running' AND source_id IN(SELECT id FROM communication_sources WHERE enabled AND NOT removal_pending)")
                    .bind(doc.id).bind(doc.version).bind(doc.summary_attempts).bind(LEASE_SECONDS).execute(&state.pool).await?;
                if renewed.rows_affected() == 0 { return Ok(true); }
            }
        }
    };
    if let Some(error) = outcome.error
        && outcome.summary.items.is_empty()
    {
        fail(state, &doc, error, outcome.retryable).await?;
        return Ok(true);
    }
    // 生成期间可能修正、撤回、暂停或手工重排；旧领取不能覆盖新版本或新执行。
    let _guard = state.communications.lock().await;
    let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM communication_documents d JOIN communication_sources s ON s.id=d.source_id WHERE d.id=$1 AND d.version=$2 AND d.summary_attempts=$3 AND d.summary_status='running' AND s.enabled AND NOT s.removal_pending)")
        .bind(doc.id).bind(doc.version).bind(doc.summary_attempts).fetch_one(&state.pool).await?;
    if !active {
        return Ok(true);
    }
    let mut result = outcome.summary;
    result.image_notes = super::images::notes(state, &doc).await?;
    let mut current = doc.clone();
    current.summary_hash = Some(store::write_summary(state, &doc, &result)?);
    let status = if outcome.error.is_some() {
        "partial"
    } else {
        "ready"
    };
    sqlx::query("UPDATE communication_documents SET summary_hash=$2,summary_error=$3,summary_status=$4 WHERE id=$1 AND version=$5 AND summary_attempts=$6 AND summary_status='running'")
        .bind(doc.id).bind(&current.summary_hash).bind(outcome.error).bind(status).bind(doc.version).bind(doc.summary_attempts).execute(&state.pool).await?;
    store::collect(state, &current)?;
    Ok(true)
}

/// 故障只更新对应领取，语义错误立即终止，临时网络或供应商故障最多三次。
async fn fail(
    state: &AppState,
    doc: &Document,
    code: &'static str,
    retryable: bool,
) -> ApiResult<()> {
    let status = if retryable && doc.summary_attempts < MAX_ATTEMPTS {
        "retry_wait"
    } else {
        "failed"
    };
    tracing::warn!(document_id=%doc.id, version=doc.version, attempt=doc.summary_attempts, code, status, "沟通整理执行失败");
    sqlx::query("UPDATE communication_documents SET summary_error=$2,summary_status=$3,next_summary=now()+make_interval(secs=>$4) WHERE id=$1 AND version=$5 AND summary_attempts=$6 AND summary_status='running'")
        .bind(doc.id).bind(code).bind(status).bind(RETRY_SECONDS * f64::from(doc.summary_attempts))
        .bind(doc.version).bind(doc.summary_attempts).execute(&state.pool).await?;
    Ok(())
}

/// 手动重排绑定用户看到的版本，重复点击不能产生并行摘要或覆盖新原文。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Retry {
    /// 当前详情页的文档版本。
    version: i64,
}
/// 只对停止重试或部分完成的资料重排；刷新详情仍是纯读取操作。
pub(super) async fn retry(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
    Json(input): Json<Retry>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    configured(&state)?;
    let _guard = state.communications.lock().await;
    let doc: Option<Document> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {DOCUMENT_COLUMNS} FROM communication_documents WHERE id=$1"
    )))
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    let doc = doc.ok_or(ApiError(StatusCode::NOT_FOUND, "communication_not_found"))?;
    if doc.version != input.version {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "communication_source_changed",
        ));
    }
    let enabled: bool = sqlx::query_scalar(
        "SELECT enabled AND NOT removal_pending FROM communication_sources WHERE id=$1",
    )
    .bind(doc.source_id)
    .fetch_one(&state.pool)
    .await?;
    if !enabled || doc.extraction_version != 1 {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "communication_summary_source_inactive",
        ));
    }
    if matches!(
        doc.summary_status.as_str(),
        "pending" | "running" | "retry_wait"
    ) {
        return Ok(Json(
            json!({"status":doc.summary_status,"version":doc.version}),
        ));
    }
    if !matches!(doc.summary_status.as_str(), "partial" | "failed") {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "communication_summary_retry_unavailable",
        ));
    }
    // 摘要序号可能变化，沿用原文修正的取消边界，不让旧提醒重新绑定到新条目。
    dependencies::cancel(&state, Some(id), None).await?;
    if doc.summary_hash.is_some() {
        dependencies::invalidate_context(&state).await?;
    }
    search::remove_vector(&state, id).await?;
    // 版本触发器同时清空旧摘要、错误和重试次数；事务提交前不会对外出现半更新状态。
    sqlx::query("UPDATE communication_documents SET version=version+1 WHERE id=$1 AND version=$2")
        .bind(id)
        .bind(doc.version)
        .execute(&state.pool)
        .await?;
    Ok(Json(json!({"status":"pending","version":doc.version+1})))
}
