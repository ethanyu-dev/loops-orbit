use super::{COLUMNS, Entry, MAX_ENTRIES, validate_tags, validate_text};
use crate::{
    AppState,
    auth::{self, Identity},
    error::{ApiError, ApiResult},
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

/// 所有管理操作只面向本人；第三方只能经模型获得已发布正文。
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(index).post(create))
        .route("/{id}", put(update).delete(remove))
        .route("/snapshots/{id}", get(snapshot))
        .route("/retry", axum::routing::post(retry))
        .route(
            "/extractions",
            axum::routing::post(super::extractions::create),
        )
        .route("/extractions/{id}", get(super::extractions::status))
}

/// 有界分页避免把原始证据库一次送到浏览器。
#[derive(Default, Deserialize)]
pub struct Filter {
    /// 可选状态；空表示全部。
    #[serde(default)]
    status: String,
    /// 标题及正文的字面查询。
    #[serde(default)]
    q: String,
    /// 固定每页二十条。
    #[serde(default)]
    offset: i64,
}

/// 管理员列表附独立证据摘录及扫描日期；检索入口不会复用此投影。
async fn index(
    State(state): State<AppState>,
    identity: Identity,
    Query(filter): Query<Filter>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    if filter.offset < 0
        || filter.q.chars().count() > 200
        || !matches!(
            filter.status.as_str(),
            "" | "candidate" | "published" | "rejected" | "revoked"
        )
    {
        return Err(invalid());
    }
    let total:i64=sqlx::query_scalar("SELECT count(*) FROM knowledge_entries WHERE ($1='' OR status=$1) AND ($2='' OR strpos(lower(title||' '||content),lower($2))>0)")
        .bind(&filter.status).bind(&filter.q).fetch_one(&state.pool).await?;
    let rows:Vec<Entry>=sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {COLUMNS} FROM knowledge_entries WHERE ($1='' OR status=$1) AND ($2='' OR strpos(lower(title||' '||content),lower($2))>0) ORDER BY created_at DESC,id LIMIT 20 OFFSET $3")))
        .bind(&filter.status).bind(&filter.q).bind(filter.offset).fetch_all(&state.pool).await?;
    let items = rows;
    let jobs:Value=sqlx::query_scalar("SELECT jsonb_build_object('pending',count(*) FILTER(WHERE status IN ('queued','running')),'failed',count(*) FILTER(WHERE status='failed'),'skipped',COALESCE(sum(skipped_count),0)) FROM knowledge_jobs")
        .fetch_one(&state.pool).await?;
    let rag:Value=sqlx::query_scalar("SELECT jsonb_build_object('chunks',count(*),'embedded',count(*) FILTER(WHERE embedding_version=$1),'retrying',count(*) FILTER(WHERE embedding_failures>0)) FROM rag_eligible")
        .bind(state.config.memory.as_ref().and_then(|m|m.embedding.as_ref()).map(|e|e.version())).fetch_one(&state.pool).await?;
    Ok(Json(
        json!({"rag":rag,"items":items,"total":total,"offset":filter.offset,"has_more":filter.offset+20<total,"jobs":jobs,"extraction_enabled":state.config.communications.is_some() && state.config.memory.is_some()}),
    ))
}

/// 手工录入和候选编辑均需要明确状态；模型没有调用这些接口的工具。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    /// 仅编辑时要求提供已读取版本。
    version: Option<i64>,
    /// 对外显示的主题。
    title: String,
    /// 对外正文，不包括私人证据。
    content: String,
    /// 可检索的公开标签，不得承载私人来源信息。
    #[serde(default)]
    tags: Vec<String>,
    /// published 明确代表本人批准，保存草稿须传 candidate。
    status: String,
}
impl Input {
    /// 发布状态只由本人明确提交，不由提取模型决定。
    fn validate(&self) -> ApiResult<()> {
        validate_text(&self.title, &self.content)?;
        validate_tags(&self.tags)?;
        if !matches!(
            self.status.as_str(),
            "candidate" | "published" | "rejected" | "revoked"
        ) {
            return Err(invalid());
        }
        Ok(())
    }
}

/// 手工知识由本人承担确认责任，与自动候选使用同一发布入口。
async fn create(
    State(state): State<AppState>,
    identity: Identity,
    Json(input): Json<Input>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    input.validate()?;
    let _guard = state.communications.lock().await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM knowledge_entries")
        .fetch_one(&state.pool)
        .await?;
    if count >= MAX_ENTRIES {
        return Err(ApiError(StatusCode::CONFLICT, "knowledge_limit"));
    }
    let id = Uuid::new_v4();
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "INSERT INTO knowledge_entries(id,title,content,status,fingerprint,tags) VALUES($1,$2,$3,$4,$5,$6)",
    )
    .bind(id)
    .bind(input.title.trim())
    .bind(input.content.trim())
    .bind(input.status)
    .bind(auth::hash(&input.content))
    .bind(&input.tags)
    .execute(&mut *tx)
    .await?;
    crate::rag::index::published(&mut tx, id).await?;
    tx.commit().await?;
    Ok(Json(json!({"id":id,"version":1})))
}

/// 确认覆盖编辑后的正文，原始证据不可被客户端替换；版本冲突须重新阅读。
async fn update(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
    Json(input): Json<Input>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    input.validate()?;
    let _guard = state.communications.lock().await;
    let entry: Entry = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM knowledge_entries WHERE id=$1"
    )))
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ApiError(StatusCode::NOT_FOUND, "knowledge_not_found"))?;
    if input.version != Some(entry.version) {
        return Err(changed());
    }
    let mut tx = state.pool.begin().await?;
    let updated=sqlx::query("UPDATE knowledge_entries SET title=$3,content=$4,status=$5,tags=$6,version=version+1,updated_at=now() WHERE id=$1 AND version=$2")
        .bind(id).bind(entry.version).bind(input.title.trim()).bind(input.content.trim()).bind(input.status).bind(&input.tags).execute(&mut *tx).await?;
    if updated.rows_affected() == 0 {
        return Err(changed());
    }
    crate::rag::index::published(&mut tx, id).await?;
    tx.commit().await?;
    Ok(Json(json!({"id":id,"version":entry.version+1})))
}

/// 删除只由本人发起，发布过的内容同时使旧上下文和等待投递结果失效。
async fn remove(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    let _guard = state.communications.lock().await;
    sqlx::query("DELETE FROM knowledge_entries WHERE id=$1")
        .bind(id)
        .execute(&state.pool)
        .await?;
    Ok(Json(json!({"ok":true})))
}

/// 只重试当前失败分块，不重复发布、重新抽取成功条目或恢复已拒绝候选。
async fn retry(State(state): State<AppState>, identity: Identity) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    sqlx::query("UPDATE knowledge_jobs SET status='queued',attempts=0,error=NULL,available_at=now() WHERE status='failed'")
        .execute(&state.pool).await?;
    Ok(Json(json!({"ok":true})))
}
/// 输入错误只向界面提供固定类别。
fn invalid() -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, "invalid_knowledge")
}
/// 乐观并发不覆盖其他管理窗口的审批。
fn changed() -> ApiError {
    ApiError(StatusCode::CONFLICT, "knowledge_changed")
}

/// 快照只对本人开放，返回独立保存的原文；不依赖已删除的源文件。
async fn snapshot(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    let value:Option<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'day',source_day,'source',source_label,'messages',messages) FROM knowledge_snapshots WHERE id=$1")
        .bind(id).fetch_optional(&state.pool).await?;
    Ok(Json(value.ok_or(ApiError(
        StatusCode::NOT_FOUND,
        "knowledge_snapshot_not_found",
    ))?))
}
