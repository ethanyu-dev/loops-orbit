use super::{store, unavailable};
use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use sqlx::{Postgres, Transaction};

// 与任务完成/发送共用行锁，使遗忘边界包含已经提交的旧输入。
const LOCK_SQL: &str = "SELECT id FROM conversations WHERE owner=$1 ORDER BY id FOR UPDATE";
const CANCEL_SQL: &str = include_str!("../sql/memory_cancel.sql");

/// 原文边界比数据库新时，修复文件已提交但数据库尚未提交的崩溃窗口。
pub(super) async fn sync_boundary(state: &AppState, owner: &str) -> ApiResult<()> {
    let config = state
        .config
        .memory
        .as_ref()
        .ok_or(ApiError(StatusCode::CONFLICT, "memory_disabled"))?;
    let through = store::boundary(&config.directory, owner).map_err(unavailable)?;
    sqlx::query("INSERT INTO memory_owners(owner) VALUES($1) ON CONFLICT DO NOTHING")
        .bind(owner)
        .execute(&state.pool)
        .await?;
    let previous: i64 =
        sqlx::query_scalar("SELECT forgotten_through FROM memory_owners WHERE owner=$1")
            .bind(owner)
            .fetch_one(&state.pool)
            .await?;
    if through <= previous {
        return Ok(());
    }
    let mut tx = state.pool.begin().await?;
    sqlx::query(LOCK_SQL).bind(owner).execute(&mut *tx).await?;
    apply(&mut tx, owner, through).await?;
    tx.commit().await?;
    Ok(())
}

/// 在读取最大序号前先锁定会话，避免把仍在提交的旧输入遗漏在遗忘边界外。
pub(crate) async fn invalidate(state: &AppState, owner: &str) -> ApiResult<()> {
    let config = state
        .config
        .memory
        .as_ref()
        .ok_or(ApiError(StatusCode::CONFLICT, "memory_disabled"))?;
    let mut tx = state.pool.begin().await?;
    sqlx::query(LOCK_SQL).bind(owner).execute(&mut *tx).await?;
    let through: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(seq),0) FROM runs")
        .fetch_one(&mut *tx)
        .await?;
    store::write_boundary(&config.directory, owner, through).map_err(unavailable)?;
    apply(&mut tx, owner, through).await?;
    tx.commit().await?;
    Ok(())
}

/// 摘要、生成租约和提取队列在同一事务失效；可查看历史不会物理删除。
async fn apply(tx: &mut Transaction<'_, Postgres>, owner: &str, through: i64) -> ApiResult<()> {
    sqlx::query(
        "UPDATE memory_owners SET forgotten_through=GREATEST(forgotten_through,$2) WHERE owner=$1",
    )
    .bind(owner)
    .bind(through)
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE conversations SET context_summary='',summary_through=GREATEST(summary_through,$2) WHERE owner=$1")
        .bind(owner).bind(through).execute(&mut **tx).await?;
    sqlx::query(CANCEL_SQL)
        .bind(owner)
        .bind(through)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE memory_jobs SET status='completed' WHERE owner=$1 AND source_seq <= $2")
        .bind(owner)
        .bind(through)
        .execute(&mut **tx)
        .await?;
    // 历史上下文失效时，旧历史发现的回访也失效；明确提醒只按其原文依赖取消。
    sqlx::query("UPDATE followup_discovery SET status='completed' WHERE owner=$1 AND run_id IN(SELECT id FROM runs WHERE seq<=$2)").bind(owner).bind(through).execute(&mut **tx).await?;
    let ids: Vec<uuid::Uuid> =
        sqlx::query_scalar(include_str!("../sql/followup_memory_boundary.sql"))
            .bind(owner)
            .bind(through)
            .fetch_all(&mut **tx)
            .await?;
    for id in ids {
        sqlx::query(include_str!("../sql/followup_cancel_outbox.sql"))
            .bind(id)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query("UPDATE messages SET context_visible=false WHERE kind='followup' AND conversation_id IN(SELECT id FROM conversations WHERE owner=$1)").bind(owner).execute(&mut **tx).await?;
    Ok(())
}
