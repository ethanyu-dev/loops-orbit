use super::{configured, dependencies, removal_jobs};
use crate::{
    AppState,
    auth::Identity,
    error::{ApiError, ApiResult},
};
use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// 弹窗预览绑定全部订阅的身份及版本，阻止确认期间新增的订阅被悄悄移除。
#[derive(sqlx::FromRow, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SubscriptionVersion {
    /// 来源主键。
    pub id: Uuid,
    /// 用户看到的启停或重新订阅版本。
    pub version: i64,
}

/// 固定长度 UUID 和版本编码使摘要无歧义，返回值不承载任何访问权限。
pub(super) fn revision(sources: &[SubscriptionVersion]) -> String {
    let mut hash = Sha256::new();
    for source in sources {
        hash.update(source.id.as_bytes());
        hash.update(source.version.to_be_bytes());
    }
    hex::encode(hash.finalize())
}

/// 全部入口绑定列表快照；单项操作绑定来源版本，也可清理之前保留的资料。
#[derive(Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
enum Selection {
    /// 移除当前全部订阅，包含暂停项，不包含已移除并保留资料的来源。
    All { revision: String },
    /// 移除或删除一项来源。
    One { id: Uuid, version: i64 },
    /// 仅清理勾选的历史来源，包含跨页选择但不包含后来新增的来源。
    Retained { sources: Vec<SubscriptionVersion> },
}

/// 删除必须明确勾选；省略选项时只移除订阅并保留资料。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemoveRequest {
    /// 弹窗确认的范围。
    selection: Selection,
    /// 是否同时删除所选来源的本地资料。
    #[serde(default)]
    delete_documents: bool,
}

/// 删除返回持久化任务标识，物理清理结果通过状态快照查询。
#[derive(Serialize)]
pub(super) struct RemoveResult {
    /// 已移除订阅数量；异步删除尚未完成时为零。
    removed: usize,
    /// 清理失败、仍显示在管理列表中的来源。
    failed_ids: Vec<Uuid>,
    /// 已受理的后台删除任务，仅删除资料时存在。
    job_id: Option<Uuid>,
    /// 已入队的来源数量，不代表已经物理删除。
    queued: usize,
}

/// 先对整批来源设围栏、取消历史任务，再保留或清理资料；不修改飞书原始消息或授权。
pub(super) async fn remove(
    State(state): State<AppState>,
    identity: Identity,
    Json(input): Json<RemoveRequest>,
) -> ApiResult<(StatusCode, Json<RemoveResult>)> {
    identity.require_admin()?;
    configured(&state)?;
    let _guard = state.communications.lock().await;
    let mut tx = state.pool.begin().await?;
    // 连接行锁覆盖批量版本核对与写入，自动发现不能在两者之间恢复来源。
    sqlx::query("SELECT owner FROM communication_connections WHERE owner='admin' FOR UPDATE")
        .execute(&mut *tx)
        .await?;
    let ids = match input.selection {
        Selection::All { revision: expected } => {
            let sources: Vec<SubscriptionVersion> = sqlx::query_as("SELECT id,version FROM communication_sources WHERE owner='admin' AND subscribed ORDER BY id FOR UPDATE")
                .fetch_all(&mut *tx).await?;
            if revision(&sources) != expected {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    "communication_source_changed",
                ));
            }
            sources
                .into_iter()
                .map(|source| source.id)
                .collect::<Vec<_>>()
        }
        Selection::One { id, version } => {
            let current: Option<i64> = sqlx::query_scalar("SELECT version FROM communication_sources WHERE id=$1 AND owner='admin' AND NOT removal_pending FOR UPDATE")
                .bind(id).fetch_optional(&mut *tx).await?;
            if current != Some(version) {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    "communication_source_changed",
                ));
            }
            vec![id]
        }
        Selection::Retained { sources } => {
            if !input.delete_documents {
                return Err(ApiError(
                    StatusCode::BAD_REQUEST,
                    "invalid_communication_selection",
                ));
            }
            super::retained::selected(&mut tx, sources).await?
        }
    };
    if ids.is_empty() {
        return Ok((
            StatusCode::OK,
            Json(RemoveResult {
                removed: 0,
                failed_ids: vec![],
                job_id: None,
                queued: 0,
            }),
        ));
    }
    sqlx::query("INSERT INTO communication_exclusions(owner,chat_id) SELECT owner,chat_id FROM communication_sources WHERE id=ANY($1) ON CONFLICT DO NOTHING")
        .bind(&ids).execute(&mut *tx).await?;
    sqlx::query("UPDATE communication_sources SET enabled=false,subscribed=false,version=version+1,page_token='',window_start=NULL,window_end=NULL,error=NULL WHERE id=ANY($1)")
        .bind(&ids).execute(&mut *tx).await?;
    sqlx::query("UPDATE communication_history_jobs SET status='cancelled',version=version+1,page_token='',error=NULL WHERE source_id=ANY($1) AND status<>'cancelled'")
        .bind(&ids).execute(&mut *tx).await?;
    if input.delete_documents {
        let job = removal_jobs::enqueue(&mut tx, &ids).await?;
        tx.commit().await?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(RemoveResult {
                removed: 0,
                failed_ids: vec![],
                job_id: Some(job),
                queued: ids.len(),
            }),
        ));
    }
    tx.commit().await?;
    dependencies::cancel_sources(&state, &ids).await?;
    dependencies::invalidate_context(&state).await?;
    Ok((
        StatusCode::OK,
        Json(RemoveResult {
            removed: ids.len(),
            failed_ids: vec![],
            job_id: None,
            queued: 0,
        }),
    ))
}
