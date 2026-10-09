mod extractions;
pub mod routes;
mod snapshot;
pub mod worker;

use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

// 对外只返回少量已发布正文；证据和来源只能由管理员管理接口读取。
const MAX_ENTRIES: i64 = 2000;
const CONTEXT_PROMPT: &str = include_str!("../../prompts/knowledge_context.md");
pub(super) const COLUMNS: &str = "id,title,content,tags,status,version,extraction_day,evidence,fingerprint,created_at,updated_at";

/// 独立的知识原文；确认范围只覆盖正文，证据不继承对外可见权限。
#[derive(Clone, Serialize, sqlx::FromRow)]
pub(crate) struct Entry {
    /// 本地稳定标识。
    pub id: Uuid,
    /// 可检索的主题。
    pub title: String,
    /// 本人可编辑、确认的对外正文。
    pub content: String,
    /// 经本人确认后可用于主题召回的标签。
    pub tags: Vec<String>,
    /// 候选、已发布、拒绝或撤回。
    pub status: String,
    /// 编辑与发送校验的乐观版本。
    pub version: i64,
    /// 产生候选的扫描日期；不与沟通文件建立引用，手工录入为空。
    pub extraction_day: Option<chrono::NaiveDate>,
    /// 仅管理员可见的逐字证据。
    pub evidence: Value,
    /// 自动抽取重放去重键，不随人工编辑改变。
    pub fingerprint: String,
    /// 首次提取或手工录入时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 最近人工变更时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 引文必须精确对应同批原消息，不能用模型归纳替代证据。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Quote {
    /// 仅在提取时校验摘录，落库前丢弃。
    pub message_id: String,
    /// 原消息中的连续片段。
    pub quote: String,
}

/// 只接受有界独立正文，不把发布状态交给模型输出。
pub(super) fn validate_text(title: &str, content: &str) -> ApiResult<()> {
    if title.trim().is_empty()
        || title.chars().count() > 120
        || content.trim().is_empty()
        || content.chars().count() > 2000
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_knowledge"));
    }
    Ok(())
}

/// 对外检索结果不携带任何原始证据，避免后续调用误用管理员投影。
#[derive(Clone)]
pub(crate) struct Published {
    /// 已确认知识标识。
    pub id: Uuid,
    /// 已确认版本。
    pub version: i64,
    /// 对外标题。
    pub title: String,
    /// 对外正文。
    pub content: String,
}

/// 只投影已发布分块；私有召回结果绝不复用于接管证据。
pub(crate) fn published(hits: &[crate::rag::Hit]) -> Vec<Published> {
    hits.iter()
        .filter(|h| h.scope == "published")
        .filter_map(|h| {
            Some(Published {
                id: h.knowledge_id?,
                version: h.revision,
                title: h.title.clone(),
                content: h.content.clone(),
            })
        })
        .collect()
}

/// 共用 PG 词法和向量检索，第三方始终没有私有范围。
pub(crate) async fn search(state: &AppState, query: &str) -> ApiResult<Vec<Published>> {
    Ok(published(&crate::rag::retrieve(state, query, None).await?))
}

/// 标签属于对外知识的一部分，编辑后必须重新确认。
pub(super) fn validate_tags(tags: &[String]) -> ApiResult<()> {
    if tags.len() > 8
        || tags
            .iter()
            .any(|t| t.trim().is_empty() || t.chars().count() > 32)
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_knowledge"));
    }
    Ok(())
}

/// 对外投影主动剥离原文、发送者、来源和内部 ID，所有直接对话复用同一知识正文。
pub(crate) fn context(entries: &[Published]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    Some(format!(
        "{CONTEXT_PROMPT}\n{}",
        json!(
            entries
                .iter()
                .map(|e| json!({"title":e.title,"content":e.content}))
                .collect::<Vec<_>>()
        )
    ))
}

/// 发送前验证知识版本与发布状态；调用方已持有沟通锁。
pub(crate) async fn current(state: &AppState, id: Uuid, version: i64) -> ApiResult<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM knowledge_entries WHERE id=$1 AND version=$2 AND status='published')")
        .bind(id).bind(version).fetch_one(&state.pool).await?)
}
