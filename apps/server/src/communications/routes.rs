use super::{
    AppState, DOCUMENT_COLUMNS, Document, SOURCE_COLUMNS, Source, client, configured, dependencies,
    oauth, search, store, unavailable,
};
use crate::{
    auth::Identity,
    error::{ApiError, ApiResult},
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{delete, get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

/// 管理入口只向管理员开放，机器人仅在运行时使用已校验账号的检索能力。
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/status", get(index))
        .route(
            "/settings",
            axum::routing::put(super::subscription::settings),
        )
        .route("/history", post(super::history::batch))
        .route("/sources/{id}/history", post(super::history::history))
        .route(
            "/documents/{id}/images/{message_id}/{index}",
            get(super::images::resource),
        )
        .route("/oauth/start", post(oauth::start))
        .route("/oauth/callback", get(oauth::callback))
        .route("/connection", delete(disconnect))
        .route("/chats", get(chats))
        .route("/sources", post(add))
        .route("/sources/{id}", axum::routing::put(update).delete(remove))
        .route("/sources/{id}/sync", post(sync_now))
        .route("/documents/{id}", get(document))
        .route("/search", get(find))
}
/// 不向浏览器返回 OAuth 令牌、过期刷新凭证或内部分页令牌。
async fn index(State(state): State<AppState>, identity: Identity) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    let connection: Option<(String, String, String, bool, i64, Option<String>)> = sqlx::query_as(
        "SELECT open_id,name,status,auto_subscribe,subscription_since,discovery_error FROM communication_connections WHERE owner='admin'",
    )
    .fetch_optional(&state.pool)
    .await?;
    let sources: Vec<Source> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {SOURCE_COLUMNS} FROM communication_sources ORDER BY label,id"
    )))
    .fetch_all(&state.pool)
    .await?;
    let documents: Vec<Document> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {DOCUMENT_COLUMNS} FROM communication_documents ORDER BY day DESC,id LIMIT 100"
    )))
    .fetch_all(&state.pool)
    .await?;
    let jobs: Vec<super::subscription::HistoryJob> = sqlx::query_as("SELECT id,source_id,start_at,end_at,snapshot_end,page_token,status,error FROM communication_history_jobs ORDER BY created_at DESC LIMIT 100").fetch_all(&state.pool).await?;
    let progress = super::search::progress(&state).await?;
    Ok(Json(
        json!({"history_jobs":jobs,"progress":progress,"enabled":state.config.communications.is_some(),"connection":connection.map(|(open_id,name,status,auto_subscribe,subscription_since,discovery_error)|json!({"open_id":open_id,"name":name,"status":status,"auto_subscribe":auto_subscribe,"subscription_since":subscription_since,"discovery_error":discovery_error})),"sources":sources,"documents":documents}),
    ))
}
/// 浏览器按页加载可见会话，供手动补录历史或添加未自动发现的会话。
#[derive(Deserialize)]
struct ChatPage {
    /// 上游不透明游标。
    #[serde(default)]
    page_token: String,
}
async fn chats(
    State(state): State<AppState>,
    identity: Identity,
    Query(page): Query<ChatPage>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    configured(&state)?;
    if page.page_token.len() > 16384 {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "invalid_communication_source",
        ));
    }
    let token = client::access(&state).await?;
    let data = client::json_response(client::get(&state, "/im/v1/chats", &token).query(&[
        ("page_size", "50"),
        ("types", "group,p2p"),
        ("user_id_type", "open_id"),
        ("sort_type", "ByActiveTimeDesc"),
        ("page_token", page.page_token.as_str()),
    ]))
    .await?;
    let items=data["data"]["items"].as_array().ok_or_else(||unavailable("缺少会话列表"))?.iter().take(50).map(|item|json!({"chat_id":item["chat_id"],"name":item["name"],"chat_mode":item["chat_mode"]})).collect::<Vec<_>>();
    Ok(Json(
        json!({"items":items,"has_more":data["data"]["has_more"],"page_token":data["data"]["page_token"]}),
    ))
}
/// 用户选择具体会话和初次回溯天数；不支持任意上游 URL。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Add {
    /// 飞书会话 ID。
    chat_id: String,
    /// 展示名，不用于文件路径。
    label: String,
    /// 兼容已有客户端；零表示仅从现在订阅，历史走独立任务。
    #[serde(default)]
    days: i64,
}
async fn add(
    State(state): State<AppState>,
    identity: Identity,
    Json(input): Json<Add>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    configured(&state)?;
    if !input.chat_id.starts_with("oc_")
        || input.chat_id.len() > 128
        || !input
            .chat_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        || input.label.trim().is_empty()
        || input.label.chars().count() > 120
        || !(0..=30).contains(&input.days)
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "invalid_communication_source",
        ));
    }
    let _guard = state.communications.lock().await;
    let token = client::access(&state).await?;
    // 选择后验证该账号确实能够读取消息，权限错误不会进入无限重试队列。
    client::json_response(client::get(&state, "/im/v1/messages", &token).query(&[
        ("container_id_type", "chat"),
        ("container_id", input.chat_id.as_str()),
        ("page_size", "1"),
    ]))
    .await?;
    let now = chrono::Utc::now().timestamp();
    let start = now - input.days * 86400;
    let id:Uuid=sqlx::query_scalar("INSERT INTO communication_sources(id,owner,chat_id,label,start_at,watermark,day_timezone) VALUES($1,'admin',$2,$3,$4,$4,'Asia/Shanghai') ON CONFLICT(owner,chat_id) DO UPDATE SET label=excluded.label RETURNING id")
        .bind(Uuid::new_v4()).bind(&input.chat_id).bind(input.label.trim()).bind(start).fetch_one(&state.pool).await?;
    sqlx::query("DELETE FROM communication_exclusions WHERE owner='admin' AND chat_id=$1")
        .bind(input.chat_id)
        .execute(&state.pool)
        .await?;
    Ok(Json(json!({"id":id})))
}
/// 启停具有版本围栏，旧页面不能重新开启刚刚暂停的来源。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    /// 所见版本。
    version: i64,
    /// 是否继续采集与召回。
    enabled: bool,
}
async fn update(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
    Json(input): Json<Update>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    configured(&state)?;
    let _guard = state.communications.lock().await;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM communication_sources WHERE id=$1 AND version=$2)",
    )
    .bind(id)
    .bind(input.version)
    .fetch_one(&state.pool)
    .await?;
    if !exists {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "communication_source_changed",
        ));
    }
    if !input.enabled {
        dependencies::cancel(&state, None, Some(id)).await?;
        dependencies::invalidate_context(&state).await?;
    }
    sqlx::query("UPDATE communication_sources SET enabled=$2,version=version+1,page_token='',window_start=NULL,window_end=NULL,next_sync=now(),error=NULL WHERE id=$1").bind(id).bind(input.enabled).execute(&state.pool).await?;
    Ok(Json(json!({"ok":true})))
}
/// 手动重试只是提前调度，HTTP 请求不会等待整个历史导入完成。
async fn sync_now(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    configured(&state)?;
    sqlx::query("UPDATE communication_sources SET next_sync=now() WHERE id=$1 AND enabled")
        .bind(id)
        .execute(&state.pool)
        .await?;
    Ok(Json(json!({"ok":true})))
}
/// 先使来源不可见，再删除文件和向量，最后删元数据；失败时保留可重试来源。
async fn forget(state: &AppState, id: Uuid) -> ApiResult<()> {
    dependencies::cancel(state, None, Some(id)).await?;
    dependencies::invalidate_context(state).await?;
    sqlx::query("UPDATE communication_sources SET enabled=false,version=version+1 WHERE id=$1")
        .bind(id)
        .execute(&state.pool)
        .await?;
    let ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM communication_documents WHERE source_id=$1")
            .bind(id)
            .fetch_all(&state.pool)
            .await?;
    for doc in ids {
        search::remove_vector(state, doc).await?;
    }
    let directory = store::directory(state, id)?;
    if directory.exists() {
        std::fs::remove_dir_all(directory).map_err(unavailable)?;
    }
    sqlx::query("DELETE FROM communication_sources WHERE id=$1")
        .bind(id)
        .execute(&state.pool)
        .await?;
    Ok(())
}
/// 删除单个来源意味着停止采集并遗忘其资料，重新添加需由用户再次选择。
async fn remove(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    configured(&state)?;
    let _guard = state.communications.lock().await;
    sqlx::query("INSERT INTO communication_exclusions(owner,chat_id) SELECT owner,chat_id FROM communication_sources WHERE id=$1 ON CONFLICT DO NOTHING").bind(id).execute(&state.pool).await?;
    forget(&state, id).await?;
    Ok(Json(json!({"ok":true})))
}
/// 断开会清理本地凭证和导入资料；飞书侧应用授权可在飞书设置里另外撤销。
async fn disconnect(State(state): State<AppState>, identity: Identity) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    configured(&state)?;
    let _guard = state.communications.lock().await;
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM communication_sources")
        .fetch_all(&state.pool)
        .await?;
    for id in ids {
        forget(&state, id).await?;
    }
    sqlx::query("DELETE FROM communication_oauth_states")
        .execute(&state.pool)
        .await?;
    sqlx::query("DELETE FROM communication_connections")
        .execute(&state.pool)
        .await?;
    Ok(Json(json!({"ok":true})))
}
/// 分页查看原文，避免一日大量消息让浏览器或接口响应无界增长。
#[derive(Deserialize)]
struct Offset {
    /// 从零起的原文位置。
    #[serde(default)]
    offset: usize,
}
async fn document(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
    Query(page): Query<Offset>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    configured(&state)?;
    let _guard = state.communications.lock().await;
    let sql = format!("SELECT {DOCUMENT_COLUMNS} FROM communication_documents WHERE id=$1");
    let doc: Document = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "communication_not_found"))?;
    let raw = store::raw(&state, &doc)?;
    let image_notes = super::images::notes(&state, &doc).await?;
    let messages: Vec<_>=raw.iter().skip(page.offset).take(50).map(|m| {
        let mut value=serde_json::to_value(m).expect("消息可序列化");
        value["sender_name"]=json!(m.display_name());
        value["images"]=json!(super::images::keys(m).iter().enumerate().map(|(index,key)|json!({"reference_only":index>=20,"url":format!("/api/communications/documents/{}/images/{}/{}",doc.id,m.message_id,index),"description":image_notes.iter().find(|n|n.message_id==m.message_id && n.image_key==*key).and_then(|n|n.description.as_ref()),"error":image_notes.iter().find(|n|n.message_id==m.message_id && n.image_key==*key).and_then(|n|n.error.as_ref())})).collect::<Vec<_>>());
        value
    }).collect();
    let summary = store::summary(&state, &doc).ok().map(|s| {
        let mut v = json!(s);
        if let Some(items) = v["items"].as_array_mut() {
            for item in items {
                item["sender_name"] = json!(
                    raw.iter()
                        .find(|m| item["message_id"].as_str() == Some(&m.message_id))
                        .map(|m| m.display_name())
                        .unwrap_or("会话成员")
                );
            }
        }
        v
    });
    Ok(Json(
        json!({"document":doc,"summary":summary,"total":raw.len(),"messages":messages}),
    ))
}
/// 查询正文限长，与聊天检索复用同一身份边界。
#[derive(Deserialize)]
struct Search {
    /// 用户当前查找的问题。
    q: String,
}
async fn find(
    State(state): State<AppState>,
    identity: Identity,
    Query(input): Query<Search>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    Ok(Json(json!(
        search::search(&state, &identity.owner, &input.q).await?
    )))
}
