use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use serde_json::{Value, json};

/// 资源选择只暴露标题，每类最多五十项；搜索按字面匹配且排除不可读的沟通来源。
pub async fn search(state: &AppState, actor: &str, query: &str) -> ApiResult<Value> {
    super::identity::require_owner(state, actor).await?;
    if query.chars().count() > 500 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"));
    }
    let rows: Vec<Value> = sqlx::query_scalar(
        "SELECT jsonb_build_object('kind',kind,'id',id,'label',label) FROM (
            (SELECT 'conversation' AS kind,id::text AS id,title AS label FROM conversations WHERE personal_owner(owner)='admin' AND strpos(lower(title),lower($1))>0 ORDER BY created_at DESC LIMIT 50)
            UNION ALL
            (SELECT 'communication',d.id::text,s.label||' · '||d.day FROM communication_documents d JOIN communication_sources s ON s.id=d.source_id WHERE s.owner='admin' AND s.enabled AND NOT s.removal_pending AND strpos(lower(s.label||' '||d.day),lower($1))>0 ORDER BY d.day DESC LIMIT 50)
            UNION ALL
            (SELECT 'knowledge',id::text,title FROM knowledge_entries WHERE strpos(lower(title),lower($1))>0 ORDER BY updated_at DESC LIMIT 50)
        ) resources")
        .bind(query).fetch_all(&state.pool).await?;
    Ok(json!(rows))
}
