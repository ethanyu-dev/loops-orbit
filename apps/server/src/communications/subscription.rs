use super::{SOURCE_COLUMNS, Source, client, configured, unavailable};
use crate::{AppState, auth::Identity, error::ApiResult};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

// 完整发现一轮后十分钟重查，避免持续扫描空会话。
const DISCOVERY_SECONDS: f64 = 600.0;

/// 单独控制自动发现，不改变用户手工暂停或正在同步的来源。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Settings {
    /// 是否持续发现授权范围内的新会话。
    pub auto_subscribe: bool,
}
/// 保存发现偏好并使在途发现结果失效，已有来源不被隐式启停。
pub(super) async fn settings(
    State(state): State<AppState>,
    identity: Identity,
    Json(input): Json<Settings>,
) -> ApiResult<Json<Value>> {
    identity.require_admin()?;
    configured(&state)?;
    let _guard = state.communications.lock().await;
    sqlx::query("UPDATE communication_connections SET auto_subscribe=$1,subscription_since=CASE WHEN $1 AND NOT auto_subscribe THEN extract(epoch FROM now())::bigint ELSE subscription_since END,discovery_cursor='',next_discovery=now(),discovery_error=NULL,version=version+1 WHERE owner='admin'").bind(input.auto_subscribe).execute(&state.pool).await?;
    Ok(Json(json!({"ok":true})))
}
/// 一次发现一页，以连接版本围栏防止断开或关闭后在途结果重新添加来源。
pub async fn discover(state: &AppState) -> ApiResult<()> {
    let row: Option<(i64,i64,String)> = sqlx::query_as("SELECT version,subscription_since,discovery_cursor FROM communication_connections WHERE auto_subscribe AND status='active' AND next_discovery<=now()").fetch_optional(&state.pool).await?;
    let Some((version, since, cursor)) = row else {
        return Ok(());
    };
    sqlx::query("UPDATE communication_connections SET next_discovery=now()+interval '1 minute' WHERE version=$1").bind(version).execute(&state.pool).await?;
    let result = async {
        let token=client::access(state).await?;
        let value=client::json_response(client::get(state,"/im/v1/chats",&token).query(&[("types","group,p2p"),("page_size","50"),("sort_type","ByCreateTimeAsc"),("page_token",cursor.as_str())])).await?;
        let items=value["data"]["items"].as_array().filter(|items|items.len()<=50).ok_or_else(||unavailable("会话列表无效"))?;
        let more=value["data"]["has_more"].as_bool().ok_or_else(||unavailable("缺少分页标识"))?;
        let next=value["data"]["page_token"].as_str().unwrap_or("");
        if more && (next.is_empty() || next==cursor || next.len()>16384) {return Err(unavailable("游标无效"));}
        let _guard=state.communications.lock().await;
        let active: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM communication_connections WHERE version=$1 AND auto_subscribe AND status='active')").bind(version).fetch_one(&state.pool).await?;
        if !active {return Ok(())}
        let mut tx=state.pool.begin().await?;
        for item in items {
            let id=client::string(item,"chat_id")?;
            if !valid_chat(&id) {return Err(unavailable("会话标识无效"))}
            let label=item["name"].as_str().filter(|n|!n.trim().is_empty()).unwrap_or("未命名会话").chars().take(120).collect::<String>();
            sqlx::query("INSERT INTO communication_sources(id,owner,chat_id,label,start_at,watermark,day_timezone) SELECT $1,'admin',$2,$3,$4,$4,'Asia/Shanghai' WHERE NOT EXISTS(SELECT 1 FROM communication_exclusions WHERE owner='admin' AND chat_id=$2) ON CONFLICT(owner,chat_id) DO NOTHING").bind(Uuid::new_v4()).bind(id).bind(label).bind(since).execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE communication_connections SET discovery_cursor=$1,next_discovery=now()+make_interval(secs=>$2),discovery_error=NULL WHERE version=$3").bind(if more{next}else{""}).bind(if more{2.0}else{DISCOVERY_SECONDS}).bind(version).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }.await;
    if let Err(error) = result {
        sqlx::query("UPDATE communication_connections SET discovery_error=$1,discovery_cursor='',next_discovery=now()+interval '5 minutes' WHERE version=$2").bind(error.1).bind(version).execute(&state.pool).await?;
    }
    Ok(())
}
/// 上游标识只能成为 URL 路径段，不接受任意 URL 或目录。
pub(super) fn valid_chat(id: &str) -> bool {
    id.starts_with("oc_")
        && id.len() <= 128
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// 历史队列独立于增量水位；暂停后保留进度，恢复时继续。
#[derive(sqlx::FromRow, Serialize)]
pub(super) struct HistoryJob {
    /// 历史任务本地标识。
    pub id: Uuid,
    /// 取消与重新提交的版本围栏。
    pub version: i64,
    /// 所属会话，遗忘时级联清理。
    pub source_id: Uuid,
    /// 固定历史区间起点。
    pub start_at: i64,
    /// 固定历史区间排他终点。
    pub end_at: i64,
    /// 当前分页轮次的固定终点，防止今天的补录边翻页边扩大。
    pub snapshot_end: i64,
    /// 不透明上游游标，不交给浏览器。
    #[serde(skip_serializing)]
    pub page_token: String,
    /// pending/running/complete/failed。
    pub status: String,
    /// 稳定失败类别。
    pub error: Option<String>,
}
/// 从持久队列恢复一页，完成后每日复查原区间以发现编辑和撤回。
pub async fn history_step(state: &AppState) -> ApiResult<()> {
    let guard = state.communications.lock().await;
    let job: Option<HistoryJob>=sqlx::query_as("SELECT id,version,source_id,start_at,end_at,snapshot_end,page_token,status,error FROM communication_history_jobs WHERE status<>'cancelled' AND next_attempt<=now() AND source_id IN(SELECT id FROM communication_sources WHERE enabled AND day_timezone='Asia/Shanghai') AND EXISTS(SELECT 1 FROM communication_connections WHERE status='active') ORDER BY next_attempt,id LIMIT 1").fetch_optional(&state.pool).await?;
    let Some(job) = job else { return Ok(()) };
    let mut source: Source = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {SOURCE_COLUMNS} FROM communication_sources WHERE id=$1"
    )))
    .bind(job.source_id)
    .fetch_one(&state.pool)
    .await?;
    source.window_start = Some(job.start_at);
    let snapshot_end = if job.status == "complete" {
        job.end_at.min(chrono::Utc::now().timestamp())
    } else {
        job.snapshot_end
    };
    source.window_end = Some(snapshot_end);
    source.page_token = job.page_token;
    let (open_id, version): (String, i64) =
        sqlx::query_as("SELECT open_id,version FROM communication_connections WHERE owner='admin'")
            .fetch_one(&state.pool)
            .await?;
    sqlx::query("UPDATE communication_history_jobs SET status='running',snapshot_end=$2,next_attempt=now()+interval '5 minutes' WHERE id=$1").bind(job.id).bind(snapshot_end).execute(&state.pool).await?;
    drop(guard);
    if let Err(e) = super::sync::page(
        state,
        &source,
        &open_id,
        version,
        Some((job.id, job.version)),
    )
    .await
    {
        sqlx::query("UPDATE communication_history_jobs SET status='failed',page_token='',error=$2,next_attempt=now()+interval '5 minutes' WHERE id=$1 AND version=$3 AND status='running'").bind(job.id).bind(e.1).bind(job.version).execute(&state.pool).await?;
    }
    Ok(())
}
