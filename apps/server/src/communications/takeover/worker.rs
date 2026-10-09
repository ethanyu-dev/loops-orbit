use super::super::{Source, client, store::Message, sync};
use super::{AGENT_PREFIX, MAX_AGE_MS, decision, evidence, settings};
use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

// 为远端最新消息检查保留有限预算；看不到原消息时不猜测是否仍可回复。
const RECENT_MESSAGES: &str = "20";
const MAX_INPUT_CHARS: usize = 4000;
const JOB_SECONDS: u64 = 90;

/// 队列绑定三层版本，断开重连、暂停来源或改规则都令旧任务失效。
#[derive(sqlx::FromRow)]
struct Job {
    /// 稳定投递幂等键。
    id: Uuid,
    /// 所属私聊来源。
    source_id: Uuid,
    /// 归一化的原消息快照。
    message: Value,
    /// 来源订阅版本。
    source_version: i64,
    /// 用户连接版本。
    connection_version: i64,
    /// 删除再建连接也不复用的代际。
    connection_generation: Uuid,
    /// 管理员确认的话题和开关版本。
    settings_version: i64,
}
/// 只处理新鲜真人文本；本人、机器人、未知身份和旧消息均不能触发自动外发。
fn eligible(message: &Message, since_ms: i64, now: i64) -> bool {
    !message.is_me
        && !message.deleted
        && message.sender_type == "user"
        && message.sender_id_type == "open_id"
        && message.sender_id.starts_with("ou_")
        && message.message_type == "text"
        && !message.text.trim().is_empty()
        && message.text.chars().count() <= MAX_INPUT_CHARS
        && !message.text.trim_start().starts_with("[agent]")
        && message.create_time >= since_ms
        && message.create_time >= now - MAX_AGE_MS
        && message.create_time <= now
        && message.message_id.starts_with("om_")
        && message.message_id.len() <= 128
        && message
            .message_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
}
/// 增量页面在资料锁内提交后入队；历史导入不调用此入口，重放依靠唯一键去重。
pub(crate) async fn enqueue(
    state: &AppState,
    source: &Source,
    connection_version: i64,
    messages: &[Message],
) -> ApiResult<()> {
    if state.config.typesafe.is_none() {
        return Ok(());
    }
    let settings = settings::load(state).await?;
    if !settings.enabled || settings.rules_error.is_some() {
        return Ok(());
    }
    let generation: Option<Uuid> = sqlx::query_scalar("SELECT private_discovery_generation FROM communication_connections WHERE owner='admin' AND version=$1 AND status='active' AND send_authorized").bind(connection_version).fetch_optional(&state.pool).await?;
    let Some(generation) = generation else {
        return Ok(());
    };
    let now = Utc::now().timestamp_millis();
    let own_latest = messages
        .iter()
        .filter(|m| m.is_me)
        .map(|m| m.create_time)
        .max();
    for message in messages {
        if eligible(message, settings.since_ms, now)
            && own_latest.is_none_or(|at| at < message.create_time)
        {
            sqlx::query("INSERT INTO communication_takeover_jobs(id,source_id,message_id,source_version,connection_version,connection_generation,settings_version,message) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(source_id,message_id) DO NOTHING")
                .bind(Uuid::new_v4()).bind(source.id).bind(&message.message_id).bind(source.version).bind(connection_version).bind(generation).bind(settings.version).bind(json!(message)).execute(&state.pool).await?;
        }
    }
    if let Some(at) = own_latest {
        sqlx::query("UPDATE communication_takeover_jobs SET status='ignored',reason='owner_replied',updated_at=now() WHERE source_id=$1 AND status IN ('queued','evaluating') AND (message->>'create_time')::bigint <= $2")
            .bind(source.id).bind(at).execute(&state.pool).await?;
    }
    Ok(())
}
/// 每步只领取一项；崩溃后的未知发送不自动重试，避免重复代表本人发言。
pub async fn step(state: &AppState) -> ApiResult<()> {
    if state.config.typesafe.is_none() {
        return Ok(());
    }
    sqlx::query("UPDATE communication_takeover_jobs SET status=CASE WHEN status='dispatching' THEN 'unknown' ELSE 'failed' END,reason='interrupted',updated_at=now() WHERE status IN ('evaluating','dispatching') AND updated_at<now()-interval '3 minutes'").execute(&state.pool).await?;
    let settings = settings::load(state).await?;
    if !settings.enabled || settings.rules_error.is_some() {
        return Ok(());
    }
    let job: Option<Job> = sqlx::query_as("UPDATE communication_takeover_jobs SET status='evaluating',updated_at=now() WHERE id=(SELECT id FROM communication_takeover_jobs WHERE status='queued' ORDER BY created_at,id FOR UPDATE SKIP LOCKED LIMIT 1) RETURNING id,source_id,message,source_version,connection_version,connection_generation,settings_version").fetch_optional(&state.pool).await?;
    let Some(job) = job else { return Ok(()) };
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(JOB_SECONDS),
        process(state, &job),
    )
    .await;
    let outcome = match result {
        Ok(Ok(reason)) => ("ignored", reason),
        Ok(Err(error)) => ("failed", error.1),
        Err(_) => ("failed", "takeover_timeout"),
    };
    // process 成功发送已经写入 sent；仅更新未终结状态，尊重途中关闭或本人介入。
    sqlx::query("UPDATE communication_takeover_jobs SET status=CASE WHEN status='dispatching' THEN 'unknown' ELSE $2 END,reason=$3,updated_at=now() WHERE id=$1 AND status IN ('evaluating','dispatching')")
        .bind(job.id).bind(outcome.0).bind(outcome.1).execute(&state.pool).await?;
    Ok(())
}
/// 所有判断、生成和证据读取完成后才进入发送阶段。
async fn process(state: &AppState, job: &Job) -> ApiResult<&'static str> {
    let settings = settings::load(state).await?;
    if !active(state, job).await? {
        return Ok("scope_changed");
    }
    let message: Message =
        serde_json::from_value(job.message.clone()).map_err(super::super::unavailable)?;
    if !eligible(&message, settings.since_ms, Utc::now().timestamp_millis()) {
        return Ok("expired");
    }
    let topics: Vec<String> =
        serde_json::from_value(settings.topics).map_err(super::super::unavailable)?;
    let Some((topic, probability)) =
        decision::matching(state, &message.text, &topics, settings.threshold).await?
    else {
        return Ok("not_matched");
    };
    sqlx::query("UPDATE communication_takeover_jobs SET topic=$2,probability=$3 WHERE id=$1 AND status='evaluating'").bind(job.id).bind(&topic).bind(probability).execute(&state.pool).await?;
    let evidence = evidence::gather(state, &message.text, &topic).await?;
    if evidence.is_empty() {
        return Ok("no_evidence");
    }
    let mut input =
        json!({"incoming_message":message.text,"allowed_topic":topic,"evidence":evidence});
    let generated = state
        .runtime
        .takeover_answer(&input)
        .await
        .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "takeover_answer_unavailable"))?;
    let Some(answer) = evidence::validate(generated, &evidence) else {
        return Ok("unanswerable");
    };
    input["proposed_answer"] = json!(answer);
    if !decision::review(state, input, settings.threshold).await? {
        return Ok("answer_not_supported");
    }
    let token = client::access(state).await?;
    let open_id: String =
        sqlx::query_scalar("SELECT open_id FROM communication_connections WHERE owner='admin'")
            .fetch_one(&state.pool)
            .await?;
    if !fresh_remote(state, &message, &open_id, &token).await? {
        return Ok("conversation_changed");
    }
    // 发送期间与关闭、遗忘、资料修正串行；网络请求受 client 的五秒上限约束。
    let _communication = state.communications.lock().await;
    if !active(state, job).await?
        || !evidence::current(state, &evidence).await?
        || Utc::now().timestamp_millis() - message.create_time > MAX_AGE_MS
    {
        return Ok("scope_changed");
    }
    let answer = format!("{AGENT_PREFIX}{answer}");
    let updated = sqlx::query("UPDATE communication_takeover_jobs SET status='dispatching',answer=$2,updated_at=now() WHERE id=$1 AND status='evaluating'").bind(job.id).bind(&answer).execute(&state.pool).await?;
    if updated.rows_affected() != 1 {
        return Ok("scope_changed");
    }
    let base = &state.config.feishu.as_ref().expect("已验证配置").api_base;
    let sent = client::json_response(state.http.post(format!("{base}/im/v1/messages/{}/reply", message.message_id)).bearer_auth(token)
        .json(&json!({"msg_type":"text","content":json!({"text":answer}).to_string(),"uuid":job.id.to_string()}))).await?;
    let sent_id = client::string(&sent["data"], "message_id")?;
    sqlx::query("UPDATE communication_takeover_jobs SET status='sent',sent_message_id=$2,reason=NULL,updated_at=now() WHERE id=$1 AND status='dispatching'").bind(job.id).bind(sent_id).execute(&state.pool).await?;
    Ok("sent")
}
/// 本人或对方的新输入优先于旧答案；分页无法覆盖原消息时保守静默。
async fn fresh_remote(
    state: &AppState,
    message: &Message,
    open_id: &str,
    token: &str,
) -> ApiResult<bool> {
    let response = client::json_response(client::get(state, "/im/v1/messages", token).query(&[
        ("container_id_type", "chat"),
        ("container_id", message.chat_id.as_str()),
        ("sort_type", "ByCreateTimeDesc"),
        ("page_size", RECENT_MESSAGES),
    ]))
    .await?;
    let items = response["data"]["items"]
        .as_array()
        .filter(|items| items.len() <= 20)
        .ok_or_else(|| super::super::unavailable("缺少最新消息"))?;
    let mut found = false;
    for item in items {
        let current = sync::normalize(item, &message.chat_id, open_id)?;
        if current.message_id == message.message_id {
            found = !current.deleted
                && current.text == message.text
                && current.sender_id == message.sender_id
                && current.update_time == message.update_time
                && current.create_time == message.create_time
                && current.message_type == "text"
                && current.sender_type == "user"
                && current.sender_id_type == "open_id";
        } else if current.create_time >= message.create_time && current.sender_type == "user" {
            return Ok(false);
        }
    }
    Ok(found)
}
/// 发送资格始终来自数据库真实身份和订阅，不允许消息文本选择账号或目标会话。
async fn active(state: &AppState, job: &Job) -> ApiResult<bool> {
    // 起草期间也可能修改文件，最终发送前必须重新读取，不能只依赖数据库旧快照。
    let settings = settings::load(state).await?;
    if settings.rules_error.is_some() || settings.version != job.settings_version {
        return Ok(false);
    }
    Ok(
        sqlx::query_scalar(include_str!("../../sql/takeover_active.sql"))
            .bind(job.id)
            .bind(job.source_id)
            .bind(job.source_version)
            .bind(job.connection_version)
            .bind(job.connection_generation)
            .bind(job.settings_version)
            .fetch_one(&state.pool)
            .await?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    // 仅验证本地入队资格，真实私聊可读范围与实时性仍需租户验收。
    #[test]
    fn excludes_self_history_bots_and_agent_messages() {
        let now = Utc::now().timestamp_millis();
        let mut message: Message = serde_json::from_value(json!({"message_id":"om_test","chat_id":"oc_test","sender_id":"ou_other","sender_id_type":"open_id","sender_type":"user","is_me":false,"create_time":now-10,"update_time":now-10,"message_type":"text","deleted":false,"text":"申请 novita 测试环境权限","payload":{}})).unwrap();
        assert!(eligible(&message, now - 100, now));
        assert!(!eligible(&message, now, now));
        assert!(!eligible(&message, 0, now + MAX_AGE_MS));
        message.is_me = true;
        assert!(!eligible(&message, 0, now));
        message.is_me = false;
        message.sender_type = "app".into();
        assert!(!eligible(&message, 0, now));
        message.sender_type = "user".into();
        message.text = "[agent] 申请 novita 测试环境权限".into();
        assert!(!eligible(&message, 0, now));
    }
}
