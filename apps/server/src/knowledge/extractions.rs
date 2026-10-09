use crate::{
    AppState,
    auth::{self, Identity},
    communications::{self, Document},
    error::{ApiError, ApiResult},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// 任务状态只返回进度和固定错误码，不暴露临时输入或原文件信息。
const TASK_COLUMNS: &str = "id,kind,scan_day,status,next_offset,skipped_count,created_count,error";

/// 手选的是当前浏览的文件版本；服务端读取完整原文，不受详情分页或图片筛选影响。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Selection {
    /// 仅在受理阶段用于定位原文，任务及候选均不保存此引用。
    document_id: Uuid,
    /// 防止用户看到的内容与实际排队内容不同。
    version: i64,
}

/// 自动和手动任务复用同一处理链路，只允许本人读取执行状态。
#[derive(Serialize, sqlx::FromRow)]
pub(super) struct Task {
    /// 与文件无关的稳定任务标识。
    id: Uuid,
    /// 区分每天自动扫描和本人手动选择。
    kind: String,
    /// 被整理内容所属的北京时间日期。
    scan_day: NaiveDate,
    /// 排队、处理中、完成或失败。
    status: String,
    /// 已处理的合格文本消息数，不包括超预算跳过项。
    next_offset: i64,
    /// 超预算消息不会伪装成成功处理。
    skipped_count: i64,
    /// 本次实际新增的候选数，重复候选不会增加此计数。
    created_count: i64,
    /// 固定错误分类，不包含上游响应。
    error: Option<String>,
}

/// 手动选择可处理已保留但暂停或取消订阅的文件；明确选择不修改自动订阅范围。
/// 受理前校验版本并复制输入，随后文件变化不影响任务和知识。
pub(super) async fn create(
    State(state): State<AppState>,
    identity: Identity,
    Json(input): Json<Selection>,
) -> ApiResult<Json<Task>> {
    identity.require_admin()?;
    communications::configured(&state)?;
    let _guard = state.communications.lock().await;
    let doc: Document = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {} FROM communication_documents WHERE id=$1 AND source_id IN(SELECT id FROM communication_sources WHERE owner=$2 AND NOT removal_pending AND day_timezone='Asia/Shanghai')", communications::DOCUMENT_COLUMNS)))
        .bind(input.document_id).bind(&identity.owner).fetch_optional(&state.pool).await?
        .ok_or(ApiError(StatusCode::NOT_FOUND,"communication_not_found"))?;
    if doc.version != input.version || doc.extraction_version != 1 {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "communication_source_changed",
        ));
    }
    let day = NaiveDate::parse_from_str(&doc.day, "%Y-%m-%d")
        .map_err(|_| ApiError(StatusCode::CONFLICT, "communication_source_changed"))?;
    // 不保存可回查文件的标识；摘要仅防止重复点击同一版本重复调用模型。
    let key = auth::hash(&format!("{}:{}:{}", doc.id, doc.version, doc.raw_hash));
    let existing: Option<Task> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {TASK_COLUMNS} FROM knowledge_jobs WHERE kind='manual' AND scope_key=$1"
    )))
    .bind(&key)
    .fetch_optional(&state.pool)
    .await?;
    if let Some(task) = existing {
        if task.status == "failed" {
            sqlx::query("UPDATE knowledge_jobs SET status='queued',attempts=0,error=NULL,available_at=now() WHERE id=$1 AND status='failed'")
                .bind(task.id).execute(&state.pool).await?;
        }
        return read(&state, task.id).await.map(Json);
    }
    let (messages, skipped) = super::snapshot::from_documents(&state, &[doc]).await?;
    let id = Uuid::new_v4();
    let id: Uuid = sqlx::query_scalar("INSERT INTO knowledge_jobs(id,kind,scope_key,scan_day,messages,skipped_count) VALUES($1,'manual',$2,$3,$4,$5) ON CONFLICT(kind,scope_key) DO UPDATE SET scope_key=excluded.scope_key RETURNING id")
        .bind(id).bind(key).bind(day).bind(messages).bind(skipped).fetch_one(&state.pool).await?;
    read(&state, id).await.map(Json)
}

/// 刷新状态不重新排队；访客不可探测任务或候选数量。
pub(super) async fn status(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Task>> {
    identity.require_admin()?;
    read(&state, id).await.map(Json)
}

/// 状态读取不打开原文，原文件删除后任务仍然可查看和继续执行。
async fn read(state: &AppState, id: Uuid) -> ApiResult<Task> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {TASK_COLUMNS} FROM knowledge_jobs WHERE id=$1"
    )))
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ApiError(
        StatusCode::NOT_FOUND,
        "knowledge_extraction_not_found",
    ))
}
