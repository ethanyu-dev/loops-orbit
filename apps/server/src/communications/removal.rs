use super::{configured, dependencies, routes};
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
#[derive(sqlx::FromRow)]
pub(super) struct SubscriptionVersion {
    /// 来源主键。
    pub id: Uuid,
    /// 用户看到的启停或重新订阅版本。
    pub version: i64,
}

/// 统一读取订阅快照；调用方写入时还需持有沟通锁。
pub(super) async fn snapshot(state: &AppState) -> ApiResult<Vec<SubscriptionVersion>> {
    Ok(sqlx::query_as("SELECT id,version FROM communication_sources WHERE owner='admin' AND subscribed ORDER BY id")
        .fetch_all(&state.pool).await?)
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

/// 磁盘删除无法回滚，部分失败必须返回并保留可重试的暂停来源。
#[derive(Serialize)]
pub(super) struct RemoveResult {
    /// 成功移除的数量。
    removed: usize,
    /// 清理失败、仍显示在管理列表中的来源。
    failed_ids: Vec<Uuid>,
}

/// 先对整批来源设围栏、取消历史任务，再保留或清理资料；不修改飞书原始消息或授权。
pub(super) async fn remove(
    State(state): State<AppState>,
    identity: Identity,
    Json(input): Json<RemoveRequest>,
) -> ApiResult<Json<RemoveResult>> {
    identity.require_admin()?;
    configured(&state)?;
    let _guard = state.communications.lock().await;
    let ids = match input.selection {
        Selection::All { revision: expected } => {
            let sources = snapshot(&state).await?;
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
            let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM communication_sources WHERE id=$1 AND version=$2 AND owner='admin')")
                .bind(id).bind(version).fetch_one(&state.pool).await?;
            if !exists {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    "communication_source_changed",
                ));
            }
            vec![id]
        }
    };
    if ids.is_empty() {
        return Ok(Json(RemoveResult {
            removed: 0,
            failed_ids: vec![],
        }));
    }
    let mut tx = state.pool.begin().await?;
    // 与私聊自动发现共用连接行锁，跨实例移除也不能被在途发现重新添加。
    sqlx::query("SELECT owner FROM communication_connections WHERE owner='admin' FOR UPDATE")
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO communication_exclusions(owner,chat_id) SELECT owner,chat_id FROM communication_sources WHERE id=ANY($1) ON CONFLICT DO NOTHING")
        .bind(&ids).execute(&mut *tx).await?;
    sqlx::query("UPDATE communication_sources SET enabled=false,version=version+1,page_token='',window_start=NULL,window_end=NULL,error=NULL WHERE id=ANY($1)")
        .bind(&ids).execute(&mut *tx).await?;
    sqlx::query("UPDATE communication_history_jobs SET status='cancelled',version=version+1,page_token='',error=NULL WHERE source_id=ANY($1) AND status<>'cancelled'")
        .bind(&ids).execute(&mut *tx).await?;
    tx.commit().await?;
    for id in &ids {
        dependencies::cancel(&state, None, Some(*id)).await?;
    }
    dependencies::invalidate_context(&state).await?;
    let mut failed_ids = vec![];
    if input.delete_documents {
        for id in &ids {
            if routes::erase_files(&state, *id).await.is_err() {
                failed_ids.push(*id);
            }
        }
    } else {
        sqlx::query("UPDATE communication_sources SET subscribed=false WHERE id=ANY($1)")
            .bind(&ids)
            .execute(&state.pool)
            .await?;
    }
    Ok(Json(RemoveResult {
        removed: ids.len() - failed_ids.len(),
        failed_ids,
    }))
}
