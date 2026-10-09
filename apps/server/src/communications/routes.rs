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
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

/// 管理入口只向管理员开放，机器人仅在运行时使用已校验账号的检索能力。
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/status", get(index))
        .route("/library/days", get(super::library::days))
        .route("/library/files", get(super::library::files))
        .route("/history", post(super::history::batch))
        .route("/history/cancel", post(super::history::cancel))
        .route("/history/progress", get(super::history::progress))
        .route("/sources/{id}/history", post(super::history::history))
        .route(
            "/documents/{id}/images/{message_id}/{index}",
            get(super::images::resource),
        )
        .route("/oauth/start", post(oauth::start))
        .route("/oauth/callback", get(oauth::callback))
        .route("/connection", delete(disconnect))
        .route("/chats", get(chats))
        .route("/chats/activity", post(super::extraction::activity))
        .route("/sources", post(add))
        .route("/sources/remove", post(super::removal::remove))
        .route("/sources/restore", post(super::retained::restore))
        .route("/removals/{id}/retry", post(super::removal_jobs::retry))
        .route("/sources/{id}", axum::routing::put(update).delete(remove))
        .route("/sources/{id}/sync", post(sync_now))
        .route("/documents/{id}", get(document))
        .route("/search", get(find))
}
/// 连接快照只包含可显示的身份和订阅策略，凭证及扫描游标不出服务端。
#[derive(sqlx::FromRow, Serialize)]
struct ConnectionSnapshot {
    /// 授权账号身份。
    open_id: String,
    /// 飞书显示名称。
    name: String,
    /// 连接可用状态。
    status: String,
    /// 兼容旧字段，群聊全量自动订阅始终关闭。
    auto_subscribe: bool,
    /// 原有授权时间边界。
    subscription_since: i64,
    /// 最近私聊发现失败的稳定分类。
    discovery_error: Option<String>,
    /// 私聊专用的自动订阅策略。
    auto_subscribe_private: bool,
}
/// 不向浏览器返回 OAuth 令牌、过期刷新凭证或内部分页令牌。
async fn index(State(state): State<AppState>, identity: Identity) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    let connection: Option<ConnectionSnapshot> = sqlx::query_as(
        "SELECT open_id,name,status,auto_subscribe,subscription_since,discovery_error,auto_subscribe_private FROM communication_connections WHERE owner='admin'",
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
    let jobs: Vec<super::subscription::HistoryJob> = sqlx::query_as("SELECT id,version,source_id,start_at,end_at,snapshot_end,page_token,status,error FROM communication_history_jobs ORDER BY created_at DESC LIMIT 100").fetch_all(&state.pool).await?;
    let progress = super::progress::read(&state).await?;
    let removals = super::removal_jobs::progress(&state).await?;
    // 版本摘要必须来自同一份展示快照，避免计数和确认范围跨请求漂移。
    let mut subscriptions = sources
        .iter()
        .filter(|source| source.subscribed)
        .map(|source| super::removal::SubscriptionVersion {
            id: source.id,
            version: source.version,
        })
        .collect::<Vec<_>>();
    subscriptions.sort_by_key(|source| source.id);
    let subscription_revision = super::removal::revision(&subscriptions);
    Ok(Json(
        json!({"subscription_revision":subscription_revision,"history_jobs":jobs,"progress":progress,"removals":removals,"enabled":state.config.communications.is_some(),"connection":connection,"sources":sources,"documents":documents}),
    ))
}
/// 浏览器按页加载候选，读取列表不会创建订阅。
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
    for item in data["data"]["items"]
        .as_array()
        .ok_or_else(|| unavailable("缺少会话列表"))?
    {
        if let (Some(id), Some(mode)) = (item["chat_id"].as_str(), item["chat_mode"].as_str())
            && matches!(mode, "p2p" | "group" | "topic")
        {
            let mode = if mode == "topic" { "group" } else { mode };
            sqlx::query("UPDATE communication_sources SET chat_mode=$2 WHERE chat_id=$1")
                .bind(id)
                .bind(mode)
                .execute(&state.pool)
                .await?;
        }
    }
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
    let mode = super::extraction::lookup_mode(&state, &input.chat_id, &token).await?;
    let now = chrono::Utc::now().timestamp();
    let start = now - input.days * 86400;
    let id: Uuid = sqlx::query_scalar(include_str!("../sql/communication_subscribe.sql"))
        .bind(Uuid::new_v4())
        .bind(&input.chat_id)
        .bind(input.label.trim())
        .bind(start)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(ApiError(
            StatusCode::CONFLICT,
            "communication_source_changed",
        ))?;
    sqlx::query("UPDATE communication_sources SET chat_mode=$2 WHERE id=$1")
        .bind(id)
        .bind(mode)
        .execute(&state.pool)
        .await?;
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
        "SELECT EXISTS(SELECT 1 FROM communication_sources WHERE id=$1 AND version=$2 AND subscribed)",
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
    erase_files(state, id).await
}

/// 调用方已经停止采集并使依赖失效，文件清理失败时保留来源以供重试。
pub(super) async fn erase_files(state: &AppState, id: Uuid) -> ApiResult<()> {
    // 按来源一次清除向量，避免每份日文件重复检测表和发起删除请求。
    let vectors: bool = sqlx::query_scalar("SELECT to_regclass('memory_vectors') IS NOT NULL")
        .fetch_one(&state.pool)
        .await?;
    if vectors {
        sqlx::query("DELETE FROM memory_vectors WHERE owner='communications:admin' AND id IN(SELECT id FROM communication_documents WHERE source_id=$1)")
            .bind(id).execute(&state.pool).await?;
    }
    let directory = store::directory(state, id)?;
    match tokio::fs::remove_dir_all(directory).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(unavailable(error)),
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
    let mut tx = state.pool.begin().await?;
    // 旧版单项删除接口也必须先提交排除记录，再释放连接锁进行文件清理。
    sqlx::query("SELECT owner FROM communication_connections WHERE owner='admin' FOR UPDATE")
        .execute(&mut *tx)
        .await?;
    let pending: bool = sqlx::query_scalar(
        "SELECT COALESCE((SELECT removal_pending FROM communication_sources WHERE id=$1),false)",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if pending {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "communication_source_changed",
        ));
    }
    sqlx::query("INSERT INTO communication_exclusions(owner,chat_id) SELECT owner,chat_id FROM communication_sources WHERE id=$1 ON CONFLICT DO NOTHING").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
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
    /// 先筛出失败图片所在消息再分页，避免失败项藏在原文后续页中。
    #[serde(default)]
    image_errors: bool,
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
    let source_label: String =
        sqlx::query_scalar("SELECT label FROM communication_sources WHERE id=$1")
            .bind(doc.source_id)
            .fetch_one(&state.pool)
            .await?;
    if doc.extraction_version != 1 {
        return Ok(Json(
            json!({"source_label":source_label,"document":doc,"summary":null,"total":0,"messages":[],"processing":true}),
        ));
    }
    let raw = store::raw(&state, &doc)?;
    let image_notes = super::images::notes(&state, &doc).await?;
    let filtered: Vec<_> = raw
        .iter()
        .filter(|message| {
            !page.image_errors
                || image_notes
                    .iter()
                    .any(|note| note.message_id == message.message_id && note.error.is_some())
        })
        .collect();
    let messages: Vec<_>=filtered.iter().skip(page.offset).take(50).map(|m| {
        let mut value=serde_json::to_value(m).expect("消息可序列化");
        value["sender_name"]=json!(m.display_name());
        value["relation"]=m.payload["relation"].clone();
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
        json!({"source_label":source_label,"document":doc,"summary":summary,"total":filtered.len(),"messages":messages}),
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
