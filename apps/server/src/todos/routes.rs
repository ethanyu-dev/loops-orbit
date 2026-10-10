use super::{Create, Update, identity, service};
use crate::{AppState, auth::Identity, error::ApiResult};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

/// 网页入口与聊天工具共享服务层，任何私有事项接口均重新验证本人。
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(index).post(create))
        .route("/{id}", get(detail).put(update))
        .route("/{id}/schedules", post(schedule))
        .route("/{id}/links", post(link))
        .route("/{id}/runs/complete", post(complete_run))
        .route("/identities", get(identities).put(binding))
        .route("/preferences", get(preferences).put(prefs))
        .route("/resources", get(resources))
}
/// 有界字面搜索与分页参数。
#[derive(Deserialize)]
struct Search {
    #[serde(default)]
    q: String,
    #[serde(default)]
    offset: i64,
    #[serde(default = "default_view")]
    view: String,
}
fn default_view() -> String {
    "all".into()
}
async fn index(
    State(state): State<AppState>,
    identity: Identity,
    Query(q): Query<Search>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        service::list(&state, &identity.owner, &q.q, q.offset, &q.view).await?,
    ))
}
async fn detail(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    Ok(Json(service::detail(&state, &identity.owner, id).await?))
}
async fn create(
    State(state): State<AppState>,
    identity: Identity,
    Json(input): Json<Create>,
) -> ApiResult<Json<Value>> {
    crate::auth::limit(&state, &format!("todos:{}", identity.owner), 30).await?;
    Ok(Json(
        service::create(&state, &identity.owner, &input, None).await?,
    ))
}
async fn update(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
    Json(input): Json<Update>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        service::update(&state, &identity.owner, id, &input, None).await?,
    ))
}
async fn schedule(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
    Json(input): Json<Value>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        service::schedule(&state, &identity.owner, id, &input, None).await?,
    ))
}
async fn link(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
    Json(input): Json<Value>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        service::link(&state, &identity.owner, id, &input, None).await?,
    ))
}
async fn identities(State(state): State<AppState>, identity: Identity) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    let rows: Vec<Value> =
        sqlx::query_scalar("SELECT to_jsonb(i) FROM personal_identities i ORDER BY owner")
            .fetch_all(&state.pool)
            .await?;
    Ok(Json(json!(rows)))
}
/// 解绑与资料采集分开操作，重新绑定仍须有有效 OAuth 身份证明。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    owner: String,
    enabled: bool,
    version: i64,
}
async fn binding(
    State(state): State<AppState>,
    identity: Identity,
    Json(input): Json<Binding>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    identity::set_binding(&state, &input.owner, input.enabled, input.version).await?;
    Ok(Json(json!({"ok":true})))
}
async fn preferences(State(state): State<AppState>, identity: Identity) -> ApiResult<Json<Value>> {
    identity::require_owner(&state, &identity.owner).await?;
    Ok(Json(json!(
        crate::followups::preferences(&state, &identity.owner).await?
    )))
}
async fn prefs(
    State(state): State<AppState>,
    identity: Identity,
    Json(input): Json<crate::followups::Preferences>,
) -> ApiResult<Json<Value>> {
    identity::require_owner(&state, &identity.owner).await?;
    crate::followups::save_preferences(&state, &identity.owner, &input).await?;
    Ok(Json(json!({"ok":true})))
}

/// 确认当前一期不会把整个周期事项结束。
async fn complete_run(
    State(state): State<AppState>,
    identity: Identity,
    Path(id): Path<Uuid>,
    Json(input): Json<Value>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        service::complete_run(&state, &identity.owner, id, &input, None).await?,
    ))
}

/// 只给本人返回有界的可关联资源标题，正文仍由各资料接口独立授权。
async fn resources(
    State(state): State<AppState>,
    identity: Identity,
    Query(q): Query<Search>,
) -> ApiResult<Json<Value>> {
    super::resources::search(&state, &identity.owner, &q.q)
        .await
        .map(Json)
}
