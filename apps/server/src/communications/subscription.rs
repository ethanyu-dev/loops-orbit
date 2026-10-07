use super::{SOURCE_COLUMNS, Source};
use crate::{AppState, error::ApiResult};
use serde::Serialize;
use uuid::Uuid;

/// 上游标识只能成为 URL 路径段，不接受任意 URL 或目录。
pub(super) fn valid_chat(id: &str) -> bool {
    id.starts_with("oc_")
        && id.len() <= 128
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// 历史队列独立于增量水位；暂停后保留进度，恢复时继续。
#[derive(sqlx::FromRow, Serialize)]
pub(super) struct HistoryJob {
    /// 历史任务本地标识。
    pub id: Uuid,
    /// 取消与重新提交的版本围栏。
    pub version: i64,
    /// 所属会话，遗忘时级联清理。
    pub source_id: Uuid,
    /// 固定历史区间起点。
    pub start_at: i64,
    /// 固定历史区间排他终点。
    pub end_at: i64,
    /// 当前分页轮次的固定终点，防止今天的补录边翻页边扩大。
    pub snapshot_end: i64,
    /// 不透明上游游标，不交给浏览器。
    #[serde(skip_serializing)]
    pub page_token: String,
    /// pending/running/complete/failed。
    pub status: String,
    /// 稳定失败类别。
    pub error: Option<String>,
}
/// 从持久队列恢复一页，完成后每日复查原区间以发现编辑和撤回。
pub async fn history_step(state: &AppState) -> ApiResult<()> {
    let guard = state.communications.lock().await;
    let job: Option<HistoryJob>=sqlx::query_as("SELECT id,version,source_id,start_at,end_at,snapshot_end,page_token,status,error FROM communication_history_jobs WHERE status<>'cancelled' AND next_attempt<=now() AND source_id IN(SELECT id FROM communication_sources WHERE enabled AND day_timezone='Asia/Shanghai') AND EXISTS(SELECT 1 FROM communication_connections WHERE status='active') ORDER BY next_attempt,id LIMIT 1").fetch_optional(&state.pool).await?;
    let Some(job) = job else { return Ok(()) };
    let mut source: Source = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {SOURCE_COLUMNS} FROM communication_sources WHERE id=$1"
    )))
    .bind(job.source_id)
    .fetch_one(&state.pool)
    .await?;
    source.window_start = Some(job.start_at);
    let snapshot_end = if job.status == "complete" {
        job.end_at.min(chrono::Utc::now().timestamp())
    } else {
        job.snapshot_end
    };
    source.window_end = Some(snapshot_end);
    source.page_token = job.page_token;
    let (open_id, version): (String, i64) =
        sqlx::query_as("SELECT open_id,version FROM communication_connections WHERE owner='admin'")
            .fetch_one(&state.pool)
            .await?;
    sqlx::query("UPDATE communication_history_jobs SET status='running',snapshot_end=$2,next_attempt=now()+interval '5 minutes' WHERE id=$1").bind(job.id).bind(snapshot_end).execute(&state.pool).await?;
    drop(guard);
    if let Err(e) = super::sync::page(
        state,
        &source,
        &open_id,
        version,
        Some((job.id, job.version)),
    )
    .await
    {
        sqlx::query("UPDATE communication_history_jobs SET status='failed',page_token='',error=$2,next_attempt=now()+interval '5 minutes' WHERE id=$1 AND version=$3 AND status='running'").bind(job.id).bind(e.1).bind(job.version).execute(&state.pool).await?;
    }
    Ok(())
}
