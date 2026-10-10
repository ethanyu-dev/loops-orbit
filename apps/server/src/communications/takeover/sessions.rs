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
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

// 真人连续半小时没有发言后，临时本人接管才允许自动结束。
pub(super) const IDLE_MS: i64 = 30 * 60 * 1000;

/// 每段私聊的控制状态；版本供页面乐观锁使用，epoch 隔离恢复前后的上下文。
#[derive(sqlx::FromRow)]
pub(super) struct Session {
    /// 当前对话片段的随机标识。
    pub epoch: Uuid,
    /// 自动、本人处理中、手动暂停或发送待核对。
    pub mode: String,
    /// 只处理此边界之后的新消息。
    pub boundary_ms: i64,
    /// 上次真人发言时间，Agent 回采不延长人工状态。
    pub last_activity_ms: i64,
    /// 临时人工状态的截止时间；手动暂停没有截止时间。
    pub human_until_ms: Option<i64>,
}
/// 所有入队、人工操作和发送都先锁这行，跨进程也不能同时改写会话。
pub(super) async fn lock(tx: &mut Transaction<'_, Postgres>, source: Uuid) -> ApiResult<Session> {
    Ok(sqlx::query_as("SELECT epoch,mode,boundary_ms,last_activity_ms,human_until_ms FROM communication_takeover_sessions WHERE source_id=$1 FOR UPDATE")
        .bind(source).fetch_one(&mut **tx).await?)
}
/// 废弃待回答轮次，保留历史审计；已开始投递的任务必须单独核对。
pub(super) async fn cancel(
    tx: &mut Transaction<'_, Postgres>,
    source: Uuid,
    reason: &str,
) -> ApiResult<()> {
    sqlx::query("UPDATE communication_takeover_jobs SET status='ignored',reason=$2,updated_at=now() WHERE source_id=$1 AND status IN ('queued','evaluating')")
        .bind(source).bind(reason).execute(&mut **tx).await?;
    sqlx::query("UPDATE communication_takeover_turns SET status='closed' WHERE source_id=$1 AND status='pending'")
        .bind(source).execute(&mut **tx).await?;
    Ok(())
}
/// 页面操作只接受固定状态与读取到的版本，不接受消息目标或发送正文。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Control {
    /// auto 恢复、human 本人接管、paused 暂停。
    mode: String,
    /// 防止另一个窗口覆盖更新后的状态。
    version: i64,
}
/// 恢复总是建立新边界，不发送暂停期间积压的问题。
pub(crate) async fn control(
    State(state): State<AppState>,
    identity: Identity,
    Path(source): Path<Uuid>,
    Json(input): Json<Control>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    if !["auto", "human", "paused"].contains(&input.mode.as_str()) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_takeover_mode"));
    }
    let _guard = state.communications.lock().await;
    let mut tx = state.pool.begin().await?;
    let current: Option<i64> = sqlx::query_scalar(
        "SELECT version FROM communication_takeover_sessions WHERE source_id=$1 FOR UPDATE",
    )
    .bind(source)
    .fetch_optional(&mut *tx)
    .await?;
    if current != Some(input.version) {
        return Err(ApiError(StatusCode::CONFLICT, "takeover_session_changed"));
    }
    let dispatching: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM communication_takeover_jobs WHERE source_id=$1 AND status='dispatching')")
        .bind(source).fetch_one(&mut *tx).await?;
    if dispatching {
        return Err(ApiError(StatusCode::CONFLICT, "takeover_dispatching"));
    }
    cancel(&mut tx, source, "session_controlled").await?;
    let now = chrono::Utc::now().timestamp_millis();
    sqlx::query("UPDATE communication_takeover_sessions SET mode=$2,version=version+1,epoch=$3,boundary_ms=$4,last_activity_ms=$4,human_until_ms=$5,topic=NULL,updated_at=now() WHERE source_id=$1")
        .bind(source).bind(&input.mode).bind(Uuid::new_v4()).bind(now)
        .bind(if input.mode=="human" {Some(now+IDLE_MS)} else {None}).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"ok":true})))
}
/// 未知投递阻止同一会话的后续自动回复；重启也不会清除此状态。
pub(super) async fn quarantine(state: &AppState) -> ApiResult<()> {
    // 用行锁与入队保持相同顺序；只处理当前状态，人工核对并恢复后不反复暂停。
    let sources: Vec<Uuid> = sqlx::query_scalar("SELECT DISTINCT source_id FROM communication_takeover_jobs WHERE status='dispatching' AND updated_at<now()-interval '3 minutes'")
        .fetch_all(&state.pool).await?;
    for source in sources {
        let mut tx = state.pool.begin().await?;
        let exists: Option<Uuid> = sqlx::query_scalar(
            "SELECT source_id FROM communication_takeover_sessions WHERE source_id=$1 FOR UPDATE",
        )
        .bind(source)
        .fetch_optional(&mut *tx)
        .await?;
        let changed = sqlx::query("UPDATE communication_takeover_jobs SET status='unknown',reason='interrupted',updated_at=now() WHERE source_id=$1 AND status='dispatching' AND updated_at<now()-interval '3 minutes'")
            .bind(source).execute(&mut *tx).await?;
        if exists.is_some() && changed.rows_affected() > 0 {
            uncertain(&mut tx, source).await?;
        }
        tx.commit().await?;
    }
    Ok(())
}
/// 调用方持有会话锁，未知结果关闭待回答轮次，须由本人显式恢复。
pub(super) async fn uncertain(tx: &mut Transaction<'_, Postgres>, source: Uuid) -> ApiResult<()> {
    cancel(tx, source, "delivery_uncertain").await?;
    sqlx::query("UPDATE communication_takeover_sessions SET mode='uncertain',version=version+1,updated_at=now() WHERE source_id=$1")
        .bind(source).execute(&mut **tx).await?;
    Ok(())
}

/// 回收崩溃的生成任务；逐会话加锁，不能与新补充的入队交错关闭新轮次。
pub(super) async fn recover_evaluations(state: &AppState) -> ApiResult<()> {
    let sources: Vec<Uuid> = sqlx::query_scalar("SELECT DISTINCT t.source_id FROM communication_takeover_turns t JOIN communication_takeover_jobs j ON j.turn_id=t.id AND j.turn_revision=t.revision WHERE t.status='pending' AND (j.status IN ('ignored','failed','sent','unknown') OR (j.status='evaluating' AND j.updated_at<now()-interval '3 minutes'))")
        .fetch_all(&state.pool).await?;
    for source in sources {
        let mut tx = state.pool.begin().await?;
        sqlx::query(
            "SELECT source_id FROM communication_takeover_sessions WHERE source_id=$1 FOR UPDATE",
        )
        .bind(source)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE communication_takeover_jobs SET status='failed',reason='interrupted',updated_at=now() WHERE source_id=$1 AND status='evaluating' AND updated_at<now()-interval '3 minutes'").bind(source).execute(&mut *tx).await?;
        sqlx::query("UPDATE communication_takeover_turns t SET status='closed' WHERE source_id=$1 AND status='pending' AND NOT EXISTS(SELECT 1 FROM communication_takeover_jobs j WHERE j.turn_id=t.id AND j.turn_revision=t.revision AND j.status IN ('queued','evaluating','dispatching'))").bind(source).execute(&mut *tx).await?;
        tx.commit().await?;
    }
    Ok(())
}
