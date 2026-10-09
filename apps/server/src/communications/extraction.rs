use super::{
    DOCUMENT_COLUMNS, Document, SOURCE_COLUMNS, Source, client, history, store, subscription, sync,
    unavailable,
};
use crate::{
    AppState,
    auth::Identity,
    error::{ApiError, ApiResult},
};
use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;

// 限制上游分页和嵌套结构，异常响应明确失败，不默认为单聊。
const MAX_CHAT_PAGES: usize = 200;
const MAX_CARD_DEPTH: usize = 32;

/// 只读取卡片可见文字区域；按钮、跳转地址、回调参数、模板变量不能充当正文。
pub(super) fn card_text(value: &Value) -> String {
    fn walk(value: &Value, output: &mut Vec<String>, depth: usize) {
        if depth > MAX_CARD_DEPTH {
            return;
        }
        match value {
            Value::String(text) => {
                if !text.trim().is_empty() {
                    output.push(text.trim().to_owned());
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, output, depth + 1);
                }
            }
            Value::Object(fields) => {
                let tag = fields.get("tag").and_then(Value::as_str).unwrap_or("");
                if matches!(
                    tag,
                    "button"
                        | "action"
                        | "input"
                        | "select_static"
                        | "select_person"
                        | "img"
                        | "at"
                ) {
                    return;
                }
                for key in [
                    "header", "title", "body", "elements", "columns", "fields", "text", "content",
                    "markdown",
                ] {
                    if let Some(child) = fields.get(key) {
                        walk(child, output, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }
    let mut parts = vec![];
    walk(value, &mut parts, 0);
    parts.join("\n")
}

/// 使用飞书用户会话列表确认单聊身份，缓存当前已有来源的类型；不相信前端传入类型。
pub(super) async fn lookup_mode(state: &AppState, chat_id: &str, token: &str) -> ApiResult<String> {
    let mut cursor = String::new();
    let mut seen = HashSet::new();
    for _ in 0..MAX_CHAT_PAGES {
        let value = client::json_response(client::get(state, "/im/v1/chats", token).query(&[
            ("types", "group,p2p"),
            ("page_size", "50"),
            ("sort_type", "ByActiveTimeDesc"),
            ("page_token", cursor.as_str()),
        ]))
        .await?;
        let items = value["data"]["items"]
            .as_array()
            .ok_or_else(|| unavailable("缺少会话列表"))?;
        let mut found = None;
        for item in items {
            if let (Some(id), Some(mode)) = (item["chat_id"].as_str(), item["chat_mode"].as_str())
                && matches!(mode, "p2p" | "group" | "topic")
            {
                let mode = if mode == "topic" { "group" } else { mode };
                sqlx::query("UPDATE communication_sources SET chat_mode=$2 WHERE chat_id=$1")
                    .bind(id)
                    .bind(mode)
                    .execute(&state.pool)
                    .await?;
                if id == chat_id {
                    found = Some(mode.to_owned());
                }
            }
        }
        if let Some(mode) = found {
            return Ok(mode);
        }
        if value["data"]["has_more"] == false {
            break;
        }
        let next = value["data"]["page_token"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 16384)
            .ok_or_else(|| unavailable("无效游标"))?;
        if !seen.insert(next.to_owned()) {
            return Err(unavailable("重复游标"));
        }
        cursor = next.to_owned();
    }
    Err(unavailable("无法确认会话类型"))
}
/// 未知类型必须先查证，不能把群聊误当成单聊而收集全部正文。
pub(super) async fn chat_mode(state: &AppState, source: &Source, token: &str) -> ApiResult<String> {
    let cached: Option<String> =
        sqlx::query_scalar("SELECT chat_mode FROM communication_sources WHERE id=$1")
            .bind(source.id)
            .fetch_one(&state.pool)
            .await?;
    match cached {
        Some(mode) if matches!(mode.as_str(), "p2p" | "group") => Ok(mode),
        _ => lookup_mode(state, &source.chat_id, token).await,
    }
}
/// 仅对同一 open_id 命名空间中的用户发送者判定本人。
fn mine(value: &Value, open_id: &str) -> bool {
    value["sender"]["id_type"] == "open_id"
        && value["sender"]["sender_type"] == "user"
        && value["sender"]["id"] == open_id
}
/// 明确 @ 本人，不把 @所有人或昵称相同视为个人关联。
fn mentions_me(value: &Value, open_id: &str) -> bool {
    value["mentions"].as_array().is_some_and(|mentions| {
        mentions.iter().any(|m| {
            (m["id_type"] == "open_id" && m["id"] == open_id) || m["id"]["open_id"] == open_id
        })
    })
}
/// 仅在通过关联筛选后展示具体原因，不由模型猜测与本人的关系。
pub(super) fn relation_label(value: &Value, mode: &str, owner: &str) -> &'static str {
    if mine(value, owner) {
        "我发送的"
    } else if mode == "p2p" {
        "与我的单聊"
    } else if mentions_me(value, owner) {
        "提及我"
    } else {
        "直接回复我"
    }
}
/// 消息 ID 只允许成为固定飞书 API 的路径段。
async fn message(state: &AppState, id: &str, token: &str, chat_id: &str) -> ApiResult<Value> {
    if !id.starts_with("om_")
        || id.len() > 128
        || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(unavailable("无效消息标识"));
    }
    let data =
        client::json_response(client::get(state, &format!("/im/v1/messages/{id}"), token)).await?;
    data["data"]["items"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|m| m["message_id"] == id && m["chat_id"] == chat_id)
        })
        .cloned()
        .ok_or_else(|| unavailable("缺少同会话消息"))
}
/// 单聊全部保留；群聊仅本人、明确提及本人或直接回复本人。跨页父消息单独核对。
pub(super) async fn related(
    state: &AppState,
    value: &Value,
    mode: &str,
    open_id: &str,
    token: &str,
    page: &[Value],
    chat_id: &str,
) -> ApiResult<bool> {
    if mode == "p2p" || mine(value, open_id) || mentions_me(value, open_id) {
        return Ok(true);
    }
    let Some(parent) = value["parent_id"].as_str().filter(|p| !p.is_empty()) else {
        return Ok(false);
    };
    let parent = match page.iter().find(|m| m["message_id"] == parent) {
        Some(value) => value.clone(),
        None => message(state, parent, token, chat_id).await?,
    };
    Ok(mine(&parent, open_id))
}

/// 日期筛选只探测是否存在消息，不订阅、不保存正文，也不宣称命中消息一定与本人相关。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Activity {
    /// 被筛选的上游会话。
    chat_id: String,
    /// 包含首尾日期的北京时间区间。
    range: history::HistoryRange,
}
pub(super) async fn activity(
    State(state): State<AppState>,
    identity: Identity,
    Json(input): Json<Activity>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    super::configured(&state)?;
    if !subscription::valid_chat(&input.chat_id) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "invalid_communication_source",
        ));
    }
    let (start, end) = history::range(&input.range)?;
    let token = client::access(&state).await?;
    let data = client::json_response(client::get(&state, "/im/v1/messages", &token).query(&[
        ("container_id_type", "chat".to_owned()),
        ("container_id", input.chat_id),
        ("start_time", start.to_string()),
        (
            "end_time",
            end.min(chrono::Utc::now().timestamp()).to_string(),
        ),
        ("page_size", "1".into()),
    ]))
    .await?;
    let items = data["data"]["items"]
        .as_array()
        .ok_or_else(|| unavailable("缺少消息列表"))?;
    Ok(Json(json!({"active": !items.is_empty()})))
}

/// 逐日升级旧快照，原文仍在但未经核对的数据不可召回；上游失败保留旧文件并稍后重试。
pub(super) async fn reprocess(state: &AppState) -> ApiResult<()> {
    // 领取与暂停共用锁，来源和连接版本必须属于同一快照；网络读取不占用此锁。
    let guard = state.communications.lock().await;
    let doc: Option<Document> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {DOCUMENT_COLUMNS} FROM communication_documents
        WHERE extraction_version=0 AND next_extraction<=now() AND source_id IN (
            SELECT s.id FROM communication_sources s
            JOIN communication_connections c ON c.owner=s.owner
            WHERE s.enabled AND s.day_timezone='Asia/Shanghai'
            AND c.status='active' AND NOT c.scope_context_pending
        ) ORDER BY day DESC LIMIT 1"
    )))
    .fetch_optional(&state.pool)
    .await?;
    let Some(doc) = doc else {
        return Ok(());
    };
    sqlx::query(
        "UPDATE communication_documents SET next_extraction=now()+interval '5 minutes' WHERE id=$1",
    )
    .bind(doc.id)
    .execute(&state.pool)
    .await?;
    let source: Source = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {SOURCE_COLUMNS} FROM communication_sources WHERE id=$1"
    )))
    .bind(doc.source_id)
    .fetch_one(&state.pool)
    .await?;
    let (open_id, version): (String, i64) =
        sqlx::query_as("SELECT open_id,version FROM communication_connections WHERE owner='admin'")
            .fetch_one(&state.pool)
            .await?;
    let raw = store::raw_unchecked(state, &doc)?;
    drop(guard);
    let token = client::access(state).await?;
    let mode = chat_mode(state, &source, &token).await?;
    let mut values = vec![];
    for old in &raw {
        // 暂停后不继续逐条拉取；已开始的 HTTP 请求仍由提交围栏拒绝写回。
        if !current(state, &doc, &source, version).await? {
            return Ok(());
        }
        let mut value = if mode == "group" && !old.deleted {
            message(state, &old.message_id, &token, &source.chat_id).await?
        } else {
            json!({"message_id":old.message_id,"chat_id":old.chat_id,"sender":{"id":old.sender_id,"id_type":old.sender_id_type,"sender_type":old.sender_type,"name":old.sender_name},"create_time":old.create_time,"update_time":old.update_time,"msg_type":old.message_type,"deleted":old.deleted,"body":old.payload["body"],"mentions":old.payload["mentions"],"parent_id":old.payload["parent_id"]})
        };
        // 单条消息接口经常没有姓名，不能覆盖已保存的可信名称。
        if value["sender"]["name"].as_str().unwrap_or("").is_empty() {
            value["sender"]["name"] = json!(old.sender_name);
        }
        values.push(value);
    }
    if !current(state, &doc, &source, version).await? {
        return Ok(());
    }
    let names = super::members::names(state, &source, &token, &values, &open_id, &mode).await;
    let mut messages = vec![];
    for value in &values {
        if !current(state, &doc, &source, version).await? {
            return Ok(());
        }
        let mut normalized = sync::normalize(value, &source.chat_id, &open_id)?;
        if normalized.sender_name.trim().is_empty()
            && normalized.sender_id_type == "open_id"
            && let Some(name) = names.get(&normalized.sender_id)
        {
            normalized.sender_name = name.clone();
        }
        normalized.payload["related"] = json!(
            !normalized.deleted
                && related(
                    state,
                    value,
                    &mode,
                    &open_id,
                    &token,
                    &values,
                    &source.chat_id
                )
                .await?
        );
        normalized.payload["relation"] = json!(relation_label(value, &mode, &open_id));
        messages.push(normalized);
    }
    let _guard = state.communications.lock().await;
    if !current(state, &doc, &source, version).await? {
        return Ok(());
    }
    // 启动已清理旧推理上下文；这里只更新隔离中的资料，保留升级后产生的新聊天。
    sync::commit_day(state, &source, &doc.day, messages).await?;
    sqlx::query(
        "UPDATE communication_documents SET extraction_version=1,next_summary=now() WHERE id=$1",
    )
    .bind(doc.id)
    .execute(&state.pool)
    .await?;
    Ok(())
}

/// 读取间隙检查任务是否仍有效，提交时在采集锁内重验，防止暂停或重新授权后的旧响应写回。
async fn current(
    state: &AppState,
    doc: &Document,
    source: &Source,
    connection_version: i64,
) -> ApiResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM communication_documents d
        JOIN communication_sources s ON s.id=d.source_id
        JOIN communication_connections c ON c.owner=s.owner
        WHERE d.id=$1 AND d.version=$2 AND d.extraction_version=0
        AND s.version=$3 AND s.enabled AND c.version=$4
        AND c.status='active' AND NOT c.scope_context_pending)",
    )
    .bind(doc.id)
    .bind(doc.version)
    .bind(source.version)
    .bind(connection_version)
    .fetch_one(&state.pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    /// 验证卡片标题、正文和布局嵌套；不代表真实租户覆盖所有卡片版本。
    #[test]
    fn visible_card_text_only() {
        let text = card_text(
            &json!({"header":{"title":{"tag":"plain_text","content":"上线确认"}},"body":{"elements":[{"tag":"markdown","content":"请周五前确认"},{"tag":"column_set","columns":[{"elements":[{"tag":"div","text":{"content":"负责人：小林"}}]}]},{"tag":"button","text":{"content":"立即操作"},"url":"https://example.com"}]},"data":{"secret":"不应提取"}}),
        );
        assert_eq!(text, "上线确认\n请周五前确认\n负责人：小林");
    }
    /// 验证只含动作的卡片不会生成虚构正文；不验证模型摘要质量。
    #[test]
    fn action_only_card_has_no_text() {
        assert!(
            card_text(&json!({"elements":[{"tag":"action","actions":[{"text":"提交"}]}]}))
                .is_empty()
        );
    }
    /// 验证本人身份和明确提及，不把相同昵称、全员提及或错误 ID 类型视为本人。
    #[test]
    fn explicit_identity_only() {
        assert!(mine(
            &json!({"sender":{"id":"ou_me","id_type":"open_id","sender_type":"user"}}),
            "ou_me"
        ));
        assert!(!mine(
            &json!({"sender":{"id":"ou_me","id_type":"user_id","sender_type":"user"}}),
            "ou_me"
        ));
        assert!(mentions_me(
            &json!({"mentions":[{"id":"ou_me","id_type":"open_id"}]}),
            "ou_me"
        ));
        assert!(!mentions_me(
            &json!({"mentions":[{"id":"all","id_type":"open_id","name":"ou_me"}]}),
            "ou_me"
        ));
    }
}
