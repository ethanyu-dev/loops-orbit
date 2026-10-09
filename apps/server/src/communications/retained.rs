use super::{configured, removal::SubscriptionVersion};
use crate::{
    AppState,
    auth::Identity,
    error::{ApiError, ApiResult},
};
use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use std::collections::BTreeMap;
use uuid::Uuid;

// 批量范围显式携带 ID 与版本，限制事务规模及单次请求体大小。
const MAX_RETAINED_SELECTION: usize = 1000;

/// 只接受页面选中的历史来源；不通过搜索条件在服务端重新扩大范围。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RestoreRequest {
    /// 勾选时的来源版本，轮询不会替用户更新确认依据。
    sources: Vec<SubscriptionVersion>,
}

/// 在写事务内锁定并核对整批历史来源；任一项失效则拒绝整批操作。
pub(super) async fn selected(
    tx: &mut Transaction<'_, Postgres>,
    sources: Vec<SubscriptionVersion>,
) -> ApiResult<Vec<Uuid>> {
    let count = sources.len();
    let expected: BTreeMap<_, _> = sources
        .into_iter()
        .map(|source| (source.id, source.version))
        .collect();
    if count == 0 || count > MAX_RETAINED_SELECTION || expected.len() != count {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "invalid_communication_selection",
        ));
    }
    let ids = expected.keys().copied().collect::<Vec<_>>();
    let current: Vec<SubscriptionVersion> = sqlx::query_as("SELECT id,version FROM communication_sources WHERE owner='admin' AND NOT subscribed AND id=ANY($1) ORDER BY id FOR UPDATE")
        .bind(&ids).fetch_all(&mut **tx).await?;
    if current.len() != count
        || current
            .iter()
            .any(|source| expected.get(&source.id) != Some(&source.version))
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "communication_source_changed",
        ));
    }
    Ok(ids)
}

/// 用户显式恢复历史来源，原子解除排除并安排新增消息同步；不重启已取消的历史任务。
pub(super) async fn restore(
    State(state): State<AppState>,
    identity: Identity,
    Json(input): Json<RestoreRequest>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    configured(&state)?;
    let _guard = state.communications.lock().await;
    let mut tx = state.pool.begin().await?;
    // 与自动发现和移除共用连接行锁，整批状态变更对其他实例同时可见。
    let active: Option<String> = sqlx::query_scalar("SELECT owner FROM communication_connections WHERE owner='admin' AND status='active' FOR UPDATE")
        .fetch_optional(&mut *tx).await?;
    if active.is_none() {
        return Err(ApiError(StatusCode::CONFLICT, "communication_reauthorize"));
    }
    let ids = selected(&mut tx, input.sources).await?;
    let now = chrono::Utc::now().timestamp();
    sqlx::query("DELETE FROM communication_exclusions e USING communication_sources s WHERE s.id=ANY($1) AND e.owner=s.owner AND e.chat_id=s.chat_id")
        .bind(&ids).execute(&mut *tx).await?;
    // 与手动重新添加一致：保留已有文件，从确认时刻接收新消息；历史缺口由历史导入补录。
    sqlx::query("UPDATE communication_sources SET subscribed=true,enabled=true,version=version+1,start_at=$2,watermark=$2,page_token='',window_start=NULL,window_end=NULL,error=NULL,next_sync=now() WHERE id=ANY($1)")
        .bind(&ids).bind(now).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"restored":ids.len()})))
}
