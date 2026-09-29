use super::{Entry, forget, list, save, search};
use crate::{
    AppState,
    auth::Identity,
    error::{ApiError, ApiResult},
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

/// 仅接受记忆正文，身份、路径、来源与时间由服务端确定。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    /// 稳定主题，修正必须沿用条目 ID。
    key: String,
    /// 常驻档案或按需召回资料。
    kind: String,
    /// 限长的 Markdown 正文。
    content: String,
    /// 可选到期时间。
    expires_at: Option<DateTime<Utc>>,
}
impl Input {
    /// 无法通过客户端伪造自动抽取来源。
    fn entry(self, id: Uuid) -> Entry {
        Entry {
            id,
            key: self.key,
            kind: self.kind,
            content: self.content,
            expires_at: self.expires_at,
            updated_at: Utc::now(),
            source_run: None,
            source_seq: 0,
            deleted: false,
        }
    }
}
/// 查询文本独立限长，避免搜索接口成为无界 embedding 代理。
#[derive(Deserialize)]
pub struct Search {
    q: Option<String>,
}

/// 管理员也只读取自身记忆，查看他人会话不等于共享其长期档案。
pub async fn index(
    State(state): State<AppState>,
    identity: Identity,
    Query(query): Query<Search>,
) -> ApiResult<Json<Value>> {
    let Some(config) = &state.config.memory else {
        return Ok(Json(json!({"enabled":false,"entries":[]})));
    };
    let query = query.q.unwrap_or_default();
    if query.chars().count() > 2000 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_memory_query"));
    }
    let entries = if query.trim().is_empty() {
        list(&state, &identity.owner)
            .await?
            .into_iter()
            .filter(|e| !e.deleted)
            .collect()
    } else {
        search(&state, &identity.owner, &query).await?
    };
    let (pending,failed): (i64,i64) = sqlx::query_as("SELECT COUNT(*) FILTER(WHERE status='queued'),COUNT(*) FILTER(WHERE status='failed') FROM memory_jobs WHERE owner=$1")
        .bind(&identity.owner).fetch_one(&state.pool).await?;
    let indexed: i64 = if let Some(embedding) = &config.embedding {
        sqlx::query_scalar("SELECT COUNT(*) FROM memory_vectors WHERE owner=$1 AND version=$2")
            .bind(&identity.owner)
            .bind(embedding.version())
            .fetch_one(&state.pool)
            .await?
    } else {
        0
    };
    Ok(Json(
        json!({"enabled":true,"semantic":config.embedding.is_some(),"auto_extract":config.auto_extract,
        "pending":pending,"failed":failed,"indexed":indexed,"entries":entries}),
    ))
}
/// 创建写入经过身份验证，不能指定其他 owner。
pub async fn create(
    State(state): State<AppState>,
    identity: Identity,
    Json(input): Json<Input>,
) -> ApiResult<Json<Entry>> {
    Ok(Json(
        save(&state, &identity.owner, input.entry(Uuid::new_v4()), false).await?,
    ))
}
/// 修正会重置模型历史边界，避免旧会话事实压过新原文。
pub async fn update(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
    Json(input): Json<Input>,
) -> ApiResult<Json<Entry>> {
    Ok(Json(
        save(&state, &identity.owner, input.entry(id), true).await?,
    ))
}
/// 遗忘只作用于当前身份，返回成功前已清理原文与向量。
pub async fn remove(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    forget(&state, &identity.owner, id).await?;
    Ok(Json(json!({"ok":true})))
}
