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
    let jobs = sqlx::query_scalar("INSERT INTO communication_history_jobs(id,source_id,start_at,end_at,snapshot_end) SELECT batch.id,batch.source_id,$3,$4,LEAST($4,extract(epoch FROM now())::bigint) FROM UNNEST($1::uuid[],$2::uuid[]) AS batch(id,source_id) ON CONFLICT(source_id,start_at,end_at) DO UPDATE SET snapshot_end=CASE WHEN communication_history_jobs.status IN('complete','cancelled') THEN LEAST(excluded.end_at,extract(epoch FROM now())::bigint) ELSE communication_history_jobs.snapshot_end END,status=CASE WHEN communication_history_jobs.status IN('complete','failed','cancelled') THEN 'pending' ELSE communication_history_jobs.status END,page_token=CASE WHEN communication_history_jobs.status='cancelled' THEN '' ELSE communication_history_jobs.page_token END,version=communication_history_jobs.version+CASE WHEN communication_history_jobs.status='cancelled' THEN 1 ELSE 0 END,next_attempt=now(),error=NULL RETURNING id")
        .bind(ids).bind(sources).bind(start).bind(end).fetch_all(&mut *tx).await?;
    tx.commit().await?;
    Ok(jobs)
}

/// 进度轮询只查询数据库元数据，不扫描原文文件，也不受最近 100 个任务列表限制。
pub(super) async fn progress(
    State(state): State<AppState>,
    identity: Identity,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    // 同一快照聚合计数和活动列表，暂停任务独立计数，避免误称已完成或正在执行。
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let counts: Value = sqlx::query_scalar(
        r#"
        SELECT jsonb_build_object(
            'total',count(*),
            'pending',count(*) FILTER (WHERE s.enabled AND j.status='pending'),
            'running',count(*) FILTER (WHERE s.enabled AND j.status='running'),
            'complete',count(*) FILTER (WHERE s.enabled AND j.status='complete'),
            'failed',count(*) FILTER (WHERE s.enabled AND j.status='failed'),
            'paused',count(*) FILTER (WHERE NOT s.enabled AND j.status<>'cancelled'),
            'cancelled',count(*) FILTER (WHERE j.status='cancelled'),
            'pages_processed',COALESCE(sum(j.pages_processed),0),
            'last_progress_at',max(j.last_progress_at)
        ) FROM communication_history_jobs j JOIN communication_sources s ON s.id=j.source_id
    "#,
    )
    .fetch_one(&mut *tx)
    .await?;
    let jobs: Vec<Value> = sqlx::query_scalar(
        r#"
        SELECT jsonb_build_object('id',j.id,'label',s.label,'enabled',s.enabled,
            'start_at',j.start_at,'end_at',j.end_at,'status',j.status,'error',j.error,
            'pages_processed',j.pages_processed,'last_progress_at',j.last_progress_at,
            'next_attempt',j.next_attempt)
        FROM communication_history_jobs j JOIN communication_sources s ON s.id=j.source_id
        ORDER BY CASE WHEN j.status='cancelled' THEN 5 WHEN NOT s.enabled THEN 4 WHEN j.status='failed' THEN 0
            WHEN j.status='running' THEN 1 WHEN j.status='pending' THEN 2 ELSE 3 END,
            j.last_progress_at DESC NULLS LAST,j.created_at DESC,j.id LIMIT 20
    "#,
    )
    .fetch_all(&mut *tx)
    .await?;
    let connected: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM communication_connections WHERE owner='admin' AND status='active')")
        .fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"counts":counts,"jobs":jobs,"connected":connected}),
    ))
}

/// 取消针对历史任务标识，不隐式暂停该会话的新消息订阅。
#[derive(Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Cancellation {
    /// 停止所有历史范围，包括已完成任务的每日复查。
    All,
    /// 停止一个明确的历史范围。
    Job { id: Uuid },
}
/// 先推进任务版本再取消，在途页提交会重新检查版本；已保存资料继续可用。
pub(super) async fn cancel(
    State(state): State<AppState>,
    identity: Identity,
    Json(input): Json<Cancellation>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    let id = match input {
        Cancellation::All => None,
        Cancellation::Job { id } => Some(id),
    };
    let _guard = state.communications.lock().await;
    let count = sqlx::query("UPDATE communication_history_jobs SET status='cancelled',version=version+1,error=NULL WHERE status<>'cancelled' AND ($1::uuid IS NULL OR id=$1)")
        .bind(id).execute(&state.pool).await?.rows_affected();
    Ok(Json(json!({"count":count})))
}
