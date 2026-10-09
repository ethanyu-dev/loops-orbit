use super::{configured, dependencies, routes};
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
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

// 状态页保留最近任务并优先展示未完成项，避免返回无界历史。
const VISIBLE_BATCHES: i64 = 20;

/// 页面轮询读取的持久化进度，刷新浏览器不会丢失任务。
#[derive(sqlx::FromRow, Serialize)]
pub(super) struct Progress {
    /// 整批删除的稳定身份。
    id: Uuid,
    /// 确认时的来源总数。
    total: i64,
    /// 尚未成功处理的排队数量。
    pending: i64,
    /// 已经完成物理清理的数量。
    complete: i64,
    /// 需用户重试的失败数量。
    failed: i64,
}

/// 调用方已锁定来源、停止采集和历史任务；任务与删除围栏同事务提交。
pub(super) async fn enqueue(tx: &mut Transaction<'_, Postgres>, ids: &[Uuid]) -> ApiResult<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO communication_removal_batches(id,owner) VALUES($1,'admin')")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE communication_sources SET removal_pending=true WHERE id=ANY($1)")
        .bind(ids)
        .execute(&mut **tx)
        .await?;
    sqlx::query("INSERT INTO communication_removal_items(batch_id,source_id,source_version) SELECT $1,id,version FROM communication_sources WHERE id=ANY($2)")
        .bind(id).bind(ids).execute(&mut **tx).await?;
    Ok(id)
}

/// 只读取元数据，慢文件删除不会阻塞页面轮询。
pub(super) async fn progress(state: &AppState) -> ApiResult<Vec<Progress>> {
    Ok(sqlx::query_as("SELECT b.id,count(*) AS total,count(*) FILTER(WHERE i.status='pending') AS pending,count(*) FILTER(WHERE i.status='complete') AS complete,count(*) FILTER(WHERE i.status='failed') AS failed FROM communication_removal_batches b JOIN communication_removal_items i ON i.batch_id=b.id WHERE b.owner='admin' GROUP BY b.id ORDER BY (count(*) FILTER(WHERE i.status<>'complete')>0) DESC,b.created_at DESC LIMIT $1")
        .bind(VISIBLE_BATCHES).fetch_all(&state.pool).await?)
}

/// 只重排失败项；成功项不重复清理，来源仍处于删除围栏内。
pub(super) async fn retry(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    configured(&state)?;
    let mut tx = state.pool.begin().await?;
    let exists: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM communication_removal_batches WHERE id=$1 AND owner='admin' FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    if exists.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "communication_not_found"));
    }
    let queued = sqlx::query("UPDATE communication_removal_items SET status='pending',error=NULL WHERE batch_id=$1 AND status='failed'")
        .bind(id).execute(&mut *tx).await?.rows_affected();
    tx.commit().await?;
    Ok(Json(json!({"queued":queued})))
}

/// 每步只准备一批或删除一个来源，释放沟通锁后让采集与其他管理请求继续执行。
pub async fn step(state: &AppState) -> ApiResult<bool> {
    configured(state)?;
    let _guard = state.communications.lock().await;
    let mut tx = state.pool.begin().await?;
    // 数据库行锁使多实例不会同时消费一批；进程退出会回滚当前项供下次继续。
    let batch: Option<(Uuid,bool)> = sqlx::query_as("SELECT id,prepared FROM communication_removal_batches b WHERE EXISTS(SELECT 1 FROM communication_removal_items i WHERE i.batch_id=b.id AND i.status='pending') ORDER BY created_at,id FOR UPDATE SKIP LOCKED LIMIT 1")
        .fetch_optional(&mut *tx).await?;
    let Some((id, prepared)) = batch else {
        return Ok(false);
    };
    if !prepared {
        let sources: Vec<Uuid> = sqlx::query_scalar(
            "SELECT source_id FROM communication_removal_items WHERE batch_id=$1",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        let result = async {
            dependencies::cancel_sources(state, &sources).await?;
            dependencies::invalidate_context(state).await
        }
        .await;
        match result {
            Ok(()) => {
                sqlx::query("UPDATE communication_removal_batches SET prepared=true WHERE id=$1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            }
            Err(error) => {
                sqlx::query("UPDATE communication_removal_items SET status='failed',error=$2 WHERE batch_id=$1 AND status='pending'").bind(id).bind(error.1).execute(&mut *tx).await?;
            }
        }
    } else {
        let (source, version): (Uuid,i64) = sqlx::query_as("SELECT source_id,source_version FROM communication_removal_items WHERE batch_id=$1 AND status='pending' ORDER BY source_id LIMIT 1")
            .bind(id).fetch_one(&mut *tx).await?;
        let current: Option<(i64, bool)> =
            sqlx::query_as("SELECT version,removal_pending FROM communication_sources WHERE id=$1")
                .bind(source)
                .fetch_optional(&mut *tx)
                .await?;
        let result = match current {
            // 文件删完、任务提交前重启时，来源不存在也视为成功，避免永久卡住。
            None => Ok(()),
            Some((v, true)) if v == version => routes::erase_files(state, source).await,
            _ => Err(ApiError(
                StatusCode::CONFLICT,
                "communication_source_changed",
            )),
        };
        let error = result.err().map(|error| error.1);
        sqlx::query("UPDATE communication_removal_items SET status=$3,error=$4 WHERE batch_id=$1 AND source_id=$2")
            .bind(id).bind(source).bind(if error.is_none() {"complete"} else {"failed"}).bind(error).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(true)
}
