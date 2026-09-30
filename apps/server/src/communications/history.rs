use super::configured;
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
use uuid::Uuid;

// 包含首尾日期最多 366 天，兼容闰年；批量选择限制仅用于约束请求体。
const MAX_HISTORY_DAYS: i64 = 366;
const MAX_SELECTED_SOURCES: usize = 10_000;

/// 日期按北京时间解释，结束日期包含整天但不能越过当前时刻。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HistoryRange {
    /// 包含的首个北京时间日期。
    pub start_date: String,
    /// 包含的最后一个北京时间日期。
    pub end_date: String,
}
/// 批量范围显式区分全部和多选，避免空数组被误当作全选。
#[derive(Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum HistorySelection {
    /// 提交时所有仍启用的已订阅会话，不包含暂停或遗忘来源。
    All,
    /// 指定会话必须全部有效，任何失效都不入队。
    Selected { source_ids: Vec<Uuid> },
}
/// 批量历史请求携带固定日期，不随后台执行时间延长范围。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BatchHistory {
    /// 来源选择方式。
    selection: HistorySelection,
    /// 北京时间的包含式日期区间。
    range: HistoryRange,
}
/// 旧单会话入口保留兼容性，与批量接口共用校验和去重逻辑。
pub(super) async fn history(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
    Json(input): Json<HistoryRange>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    let jobs = enqueue(
        &state,
        HistorySelection::Selected {
            source_ids: vec![id],
        },
        input,
    )
    .await?;
    Ok(Json(json!({"id": jobs[0]})))
}
/// 一次提交一批持久任务；返回会话数不代表消息或摘要已经处理完成。
pub(super) async fn batch(
    State(state): State<AppState>,
    identity: Identity,
    Json(input): Json<BatchHistory>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    let jobs = enqueue(&state, input.selection, input.range).await?;
    Ok(Json(json!({"count": jobs.len()})))
}
/// 日期解析独立于选源；结束日期转为次日午夜，再由入队快照限制到当前时刻。
fn range(input: &HistoryRange) -> ApiResult<(i64, i64)> {
    use chrono::TimeZone;
    let invalid = || ApiError(StatusCode::BAD_REQUEST, "invalid_history_range");
    let start =
        chrono::NaiveDate::parse_from_str(&input.start_date, "%Y-%m-%d").map_err(|_| invalid())?;
    let end =
        chrono::NaiveDate::parse_from_str(&input.end_date, "%Y-%m-%d").map_err(|_| invalid())?;
    if end < start
        || end
            > chrono::Utc::now()
                .with_timezone(&super::LOCAL_TIMEZONE)
                .date_naive()
        || (end - start).num_days() >= MAX_HISTORY_DAYS
    {
        return Err(invalid());
    }
    let second = |d: chrono::NaiveDate| {
        super::LOCAL_TIMEZONE
            .from_local_datetime(&d.and_hms_opt(0, 0, 0).expect("午夜"))
            .single()
            .map(|t| t.timestamp())
            .ok_or_else(invalid)
    };
    let start = second(start)?;
    let end = second(end.succ_opt().ok_or_else(invalid)?)?;
    if start >= end || start >= chrono::Utc::now().timestamp() {
        return Err(invalid());
    }

    Ok((start, end))
}
/// 在同一锁和事务内校验全部来源并批量写入，避免暂停竞争及部分成功。
async fn enqueue(
    state: &AppState,
    selection: HistorySelection,
    input: HistoryRange,
) -> ApiResult<Vec<Uuid>> {
    configured(state)?;
    let (start, end) = range(&input)?;
    let selected = match selection {
        HistorySelection::All => None,
        HistorySelection::Selected { mut source_ids } => {
            if source_ids.is_empty() || source_ids.len() > MAX_SELECTED_SOURCES {
                return Err(ApiError(
                    StatusCode::BAD_REQUEST,
                    "invalid_communication_source",
                ));
            }
            source_ids.sort_unstable();
            source_ids.dedup();
            Some(source_ids)
        }
    };
    let _guard = state.communications.lock().await;
    let mut tx = state.pool.begin().await?;
    let sources: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM communication_sources WHERE owner='admin' AND enabled AND ($1::uuid[] IS NULL OR id=ANY($1)) ORDER BY id")
        .bind(&selected).fetch_all(&mut *tx).await?;
    if selected.as_ref().is_some_and(|ids| ids != &sources) {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "communication_source_changed",
        ));
    }
    let ids: Vec<Uuid> = sources.iter().map(|_| Uuid::new_v4()).collect();
    // 相同窗口复用任务，保留在途游标和快照；已完成或失败的任务可由用户重新排队。
    let jobs = sqlx::query_scalar("INSERT INTO communication_history_jobs(id,source_id,start_at,end_at,snapshot_end) SELECT batch.id,batch.source_id,$3,$4,LEAST($4,extract(epoch FROM now())::bigint) FROM UNNEST($1::uuid[],$2::uuid[]) AS batch(id,source_id) ON CONFLICT(source_id,start_at,end_at) DO UPDATE SET snapshot_end=CASE WHEN communication_history_jobs.status='complete' THEN LEAST(excluded.end_at,extract(epoch FROM now())::bigint) ELSE communication_history_jobs.snapshot_end END,status=CASE WHEN communication_history_jobs.status IN('complete','failed') THEN 'pending' ELSE communication_history_jobs.status END,next_attempt=now(),error=NULL RETURNING id")
        .bind(ids).bind(sources).bind(start).bind(end).fetch_all(&mut *tx).await?;
    tx.commit().await?;
    Ok(jobs)
}
