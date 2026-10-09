pub mod index;
mod search;

use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
pub(crate) use search::retrieve;
use serde_json::{Value, json};
use uuid::Uuid;

// 独立资料边界，防止检索内容被解释成新的用户指令。
const PRIVATE_PROMPT: &str = include_str!("../../prompts/communication_context.md");

/// 检索投影只包含回答所需内容；原始快照不参与公开查询。
#[derive(Clone, sqlx::FromRow)]
pub(crate) struct Hit {
    /// 派生分块标识。
    pub id: Uuid,
    /// 已发布知识或本人私有沟通。
    pub scope: String,
    /// 已发布知识发送前校验标识。
    pub knowledge_id: Option<Uuid>,
    /// 本人读取工具所需文件标识。
    pub document_id: Option<Uuid>,
    /// 来源版本，检索与发送期间都不能使用过期数据。
    pub revision: i64,
    /// 私有原文件及摘要哈希。
    pub source_hash: String,
    /// 知识标题或会话名。
    pub title: String,
    /// 有界分块正文。
    pub content: String,
    /// 私有发送者、日期等解释信息；公开分块固定为空。
    pub payload: Value,
}

/// 给本人提供资料来源供核对，但不产生任何会话跳转链接。
pub(crate) fn private_context(hits: &[Hit]) -> Option<String> {
    let docs: Vec<_> = hits
        .iter()
        .filter(|h| h.scope == "private")
        .map(|h| {
            json!({
                "document_id":h.document_id,"version":h.revision,"source":h.title,
                "content":h.content,"metadata":h.payload
            })
        })
        .collect();
    (!docs.is_empty()).then(|| {
        format!(
            "{PRIVATE_PROMPT}\n{}",
            json!({"is_exhaustive":false,"documents":docs})
        )
    })
}

/// 数据库错误不附带私有正文、路径或供应商响应。
fn unavailable(_: impl std::fmt::Display) -> ApiError {
    ApiError(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "rag_unavailable",
    )
}

/// 内部使用的统一读接口；对外调用必须通过身份检查后的 retrieve。
async fn read_hits(state: &AppState, ids: &[Uuid]) -> ApiResult<Vec<Hit>> {
    Ok(sqlx::query_as("SELECT id,scope,knowledge_id,document_id,revision,source_hash,title,content,payload FROM rag_eligible WHERE id=ANY($1)")
        .bind(ids).fetch_all(&state.pool).await?)
}
