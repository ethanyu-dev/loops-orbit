use crate::{
    AppState,
    auth::Identity,
    error::{ApiError, ApiResult},
};
use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{rules::Rules, self_chat};

/// 设置版本同时绑定候选队列；不能将旧规则下生成的答案用于新规则。
#[derive(Clone, Serialize, sqlx::FromRow)]
pub(super) struct Settings {
    /// 仅影响新消息，默认关闭。
    pub enabled: bool,
    /// 仅由向已授权本人发送提示的响应建立，不接受前端指定会话。
    pub self_test_chat_id: Option<String>,
    /// 文件中可接管问题的快照，不能通过管理接口修改。
    pub topics: Value,
    /// Jev 肯定概率下限，不是字符串或向量相似度。
    pub threshold: f64,
    /// 并发编辑和在途任务围栏。
    pub version: i64,
    /// 本次设置生效的毫秒边界。
    pub since_ms: i64,
    /// 文件的规范化摘要，避免确认前问题范围已发生变化。
    #[sqlx(skip)]
    pub rules_revision: String,
    /// 文件异常时暂停接管，但允许继续采集和关闭开关。
    #[sqlx(skip)]
    pub rules_error: Option<&'static str>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            self_test_chat_id: None,
            topics: json!([]),
            threshold: 0.9,
            version: 0,
            since_ms: 0,
            rules_revision: String::new(),
            rules_error: None,
        }
    }
}
/// 在行锁内读取文件并更新快照；范围变化会废弃旧队列，不因读页面启用接管。
pub(super) async fn load(state: &AppState) -> ApiResult<Settings> {
    let mut tx = state.pool.begin().await?;
    let stored: Option<Settings> = sqlx::query_as("SELECT enabled,self_test_chat_id,topics,threshold,version,since_ms FROM communication_takeover_settings WHERE owner='admin' FOR UPDATE")
        .fetch_optional(&mut *tx).await?;
    let rules = Rules::load(&state.config.takeover_questions_file);
    let topics = json!(rules.questions);
    let mut settings = stored.clone().unwrap_or_default();
    if stored.is_some() && settings.topics != topics {
        settings = sqlx::query_as("UPDATE communication_takeover_settings SET topics=$1,version=version+1,since_ms=(extract(epoch FROM clock_timestamp())*1000)::bigint WHERE owner='admin' RETURNING enabled,self_test_chat_id,topics,threshold,version,since_ms")
            .bind(&topics).fetch_one(&mut *tx).await?;
        sqlx::query("UPDATE communication_takeover_jobs SET status='ignored',reason='rules_changed',updated_at=now() WHERE status IN ('queued','evaluating')").execute(&mut *tx).await?;
    }
    settings.topics = topics;
    settings.rules_revision = rules.revision;
    settings.rules_error = rules.error;
    tx.commit().await?;
    Ok(settings)
}
/// 管理员可检查最近处理记录，私聊发送者和访客不能访问。
pub(crate) async fn read(
    State(state): State<AppState>,
    identity: Identity,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    let settings = load(&state).await?;
    let authorized: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM communication_connections WHERE owner='admin' AND status='active' AND send_authorized)").fetch_one(&state.pool).await?;
    let jobs: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',j.id,'label',s.label,'question',j.message->>'text','status',j.status,'reason',j.reason,'topic',j.topic,'probability',j.probability,'answer',j.answer,'created_at',j.created_at) FROM communication_takeover_jobs j JOIN communication_sources s ON s.id=j.source_id ORDER BY j.created_at DESC LIMIT 50").fetch_all(&state.pool).await?;
    Ok(Json(
        json!({"settings":settings,"rules_file":state.config.takeover_questions_file,"configured":state.config.typesafe.is_some(),"authorized":authorized,"jobs":jobs}),
    ))
}
/// 管理接口仅保存开关和阈值；问题始终由独立文件管理。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Input {
    /// 管理员显式启用。
    enabled: bool,
    /// 显式订阅自聊并按外部提问者测试；旧客户端默认关闭。
    #[serde(default)]
    self_test_enabled: bool,
    /// 开启前确认页面展示的问题版本；关闭不依赖有效文件。
    rules_revision: Option<String>,
    /// 自动判断所需的肯定概率。
    threshold: f64,
    /// 页面读取的版本，防止覆盖另一个窗口的修改。
    version: i64,
}
/// 修改策略后取消未发送任务，并从新边界处理后续消息。
pub(crate) async fn save(
    State(state): State<AppState>,
    identity: Identity,
    Json(input): Json<Input>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    super::super::configured(&state)?;
    if !input.threshold.is_finite() || !(0.5..=1.0).contains(&input.threshold) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "invalid_takeover_settings",
        ));
    }
    let _guard = state.communications.lock().await;
    let settings = load(&state).await?;
    if input.enabled {
        if let Some(error) = settings.rules_error {
            return Err(ApiError(StatusCode::CONFLICT, error));
        }
        if input.rules_revision.as_deref() != Some(&settings.rules_revision) {
            return Err(ApiError(StatusCode::CONFLICT, "takeover_settings_changed"));
        }
    }
    // 凭证刷新也锁连接行，必须在设置事务取得行锁之前完成。
    let token = if input.enabled && input.self_test_enabled && settings.self_test_chat_id.is_none()
    {
        Some(super::super::client::access(&state).await?)
    } else {
        None
    };
    let mut tx = state.pool.begin().await?;
    let authorized: Option<bool> = sqlx::query_scalar("SELECT status='active' AND send_authorized FROM communication_connections WHERE owner='admin' FOR UPDATE").fetch_optional(&mut *tx).await?;
    let authorized = authorized.ok_or(ApiError(
        StatusCode::CONFLICT,
        "communication_not_connected",
    ))?;
    if input.enabled && (state.config.typesafe.is_none() || !authorized) {
        return Err(ApiError(StatusCode::CONFLICT, "takeover_not_ready"));
    }
    let current: Option<i64> = sqlx::query_scalar(
        "SELECT version FROM communication_takeover_settings WHERE owner='admin' FOR UPDATE",
    )
    .fetch_optional(&mut *tx)
    .await?;
    if current.unwrap_or(0) != input.version {
        return Err(ApiError(StatusCode::CONFLICT, "takeover_settings_changed"));
    }
    // 文件不参与数据库锁，提交前再核对，避免使用读取后已被替换的范围。
    let rules = Rules::load(&state.config.takeover_questions_file);
    if input.enabled && (rules.error.is_some() || rules.revision != settings.rules_revision) {
        return Err(ApiError(StatusCode::CONFLICT, "takeover_settings_changed"));
    }
    let self_test_chat_id = if input.enabled && input.self_test_enabled {
        let chat_id = match settings.self_test_chat_id {
            Some(chat_id) => chat_id,
            None => {
                let open_id: String = sqlx::query_scalar(
                    "SELECT open_id FROM communication_connections WHERE owner='admin'",
                )
                .fetch_one(&mut *tx)
                .await?;
                self_chat::bind(
                    &state,
                    &open_id,
                    token.as_deref().expect("启用前已读取凭证"),
                )
                .await?
            }
        };
        self_chat::subscribe(&mut tx, &chat_id).await?;
        Some(chat_id)
    } else {
        None
    };
    sqlx::query("INSERT INTO communication_takeover_settings(owner,enabled,topics,threshold,version,self_test_chat_id) VALUES('admin',$1,$2,$3,1,$4) ON CONFLICT(owner) DO UPDATE SET enabled=excluded.enabled,topics=excluded.topics,threshold=excluded.threshold,self_test_chat_id=excluded.self_test_chat_id,version=communication_takeover_settings.version+1,since_ms=(extract(epoch FROM clock_timestamp())*1000)::bigint")
        .bind(input.enabled).bind(json!(rules.questions)).bind(input.threshold).bind(self_test_chat_id).execute(&mut *tx).await?;
    sqlx::query("UPDATE communication_takeover_jobs SET status='ignored',reason='settings_changed',updated_at=now() WHERE status IN ('queued','evaluating')").execute(&mut *tx).await?;
    if input.enabled {
        sqlx::query("UPDATE communication_sources SET next_sync=now() WHERE enabled AND subscribed AND chat_mode='p2p'").execute(&mut *tx).await?;
        sqlx::query(
            "UPDATE communication_connections SET next_discovery=now() WHERE owner='admin'",
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Json(json!({"ok":true})))
}
