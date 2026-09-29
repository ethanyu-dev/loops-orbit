use super::{
    DOCUMENT_COLUMNS, Document, SOURCE_COLUMNS, Source, client, dependencies, store, unavailable,
};
use crate::{AppState, error::ApiResult};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};
use tokio::sync::watch;
use uuid::Uuid;

// 重叠十分钟处理边界及迟到消息；每天另做全范围复查，识别旧消息的编辑和撤回。
const OVERLAP_SECONDS: i64 = 600;
const SYNC_SECONDS: i64 = 600;
const ERROR_SECONDS: i64 = 300;
const MAX_MESSAGE_BYTES: usize = 128 * 1024;

/// 独立采集 worker 不创建 runs，不发送聊天回复；每次一页给其他来源留出机会。
pub async fn run(state: AppState, stop: watch::Receiver<bool>) {
    // 三条独立循环共享关闭信号，慢摘要或 embedding 不阻塞消息同步。
    tokio::join!(
        work_loop(&state, stop.clone(), 0),
        work_loop(&state, stop.clone(), 1),
        work_loop(&state, stop, 2)
    );
}
/// 每条循环只执行一种工作；故障统一退避，避免上游中断时快速重试。
async fn work_loop(state: &AppState, mut stop: watch::Receiver<bool>, kind: u8) {
    loop {
        if *stop.borrow() {
            return;
        }
        let mut delay = 2;
        if state.config.communications.is_some() {
            let work = async {
                match kind {
                    0 => step(state).await.map(|_| ()),
                    1 => super::summary::step(state).await.map(|_| ()),
                    _ => super::search::index_step(state).await,
                }
            };
            let result = tokio::select! { result=work=>result, _=stop.changed()=>return };
            if let Err(error) = result {
                tracing::warn!(code = error.1, kind, "沟通后台任务暂时失败");
                delay = 60;
            }
        }
        tokio::select! {_=tokio::time::sleep(Duration::from_secs(delay))=>{},_=stop.changed()=>return}
    }
}
/// 公共单步入口供真实 worker 和协议夹具共用，不使用伪造聊天队列。
pub async fn step(state: &AppState) -> ApiResult<bool> {
    super::configured(state)?;
    let sql = format!(
        "SELECT {SOURCE_COLUMNS} FROM communication_sources WHERE enabled AND next_sync<=now() AND EXISTS(SELECT 1 FROM communication_connections WHERE status='active') ORDER BY next_sync,id LIMIT 1"
    );
    let Some(mut source): Option<Source> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .fetch_optional(&state.pool)
        .await?
    else {
        return Ok(false);
    };
    let (open_id, connection_version): (String, i64) =
        sqlx::query_as("SELECT open_id,version FROM communication_connections WHERE owner='admin'")
            .fetch_one(&state.pool)
            .await?;
    let now = Utc::now().timestamp() - 2;
    if source.window_end.is_none() {
        source.window_start = Some(if source.audit_at <= Utc::now() {
            source.start_at
        } else {
            source.start_at.max(source.watermark - OVERLAP_SECONDS)
        });
        source.window_end = Some(now.max(source.window_start.unwrap_or(now) + 1));
    }
    let result = page(state, &source, &open_id, connection_version).await;
    if let Err(error) = result {
        // 页令牌可能已过期。失败重试从同一固定窗口首部开始，时间水位仍不前进。
        sqlx::query("UPDATE communication_sources SET error=$2,next_sync=now()+make_interval(secs=>$3),page_token='' WHERE id=$1 AND version=$4")
            .bind(source.id).bind(error.1).bind(ERROR_SECONDS as f64).bind(source.version).execute(&state.pool).await?;
    }
    Ok(true)
}
/// 只有完整、可解析的一页才进入提交路径，错误响应或重复令牌不移动水位。
async fn page(
    state: &AppState,
    source: &Source,
    open_id: &str,
    connection_version: i64,
) -> ApiResult<()> {
    let token = client::access(state).await?;
    let data = client::json_response(client::get(state, "/im/v1/messages", &token).query(&[
        ("container_id_type", "chat".to_owned()),
        ("container_id", source.chat_id.clone()),
        (
            "start_time",
            source.window_start.expect("已固定窗口").to_string(),
        ),
        (
            "end_time",
            source.window_end.expect("已固定窗口").to_string(),
        ),
        ("sort_type", "ByCreateTimeAsc".into()),
        ("page_size", "50".into()),
        ("page_token", source.page_token.clone()),
    ]))
    .await?;
    let items = data["data"]["items"]
        .as_array()
        .ok_or_else(|| unavailable("缺少消息列表"))?;
    if items.len() > 50 {
        return Err(unavailable("分页超限"));
    }
    let more = data["data"]["has_more"]
        .as_bool()
        .ok_or_else(|| unavailable("缺少分页状态"))?;
    let next = data["data"]["page_token"].as_str().unwrap_or("");
    if more && (next.is_empty() || next == source.page_token || next.len() > 16384) {
        return Err(unavailable("无效分页状态"));
    }
    let mut days: BTreeMap<String, Vec<store::Message>> = BTreeMap::new();
    for item in items {
        let message = normalize(item, &source.chat_id, open_id)?;
        if message.create_time / 1000 < source.start_at {
            continue;
        }
        let day = DateTime::from_timestamp_millis(message.create_time)
            .ok_or_else(|| unavailable("错误消息时间"))?
            .format("%Y-%m-%d")
            .to_string();
        days.entry(day).or_default().push(message);
    }
    let _guard = state.communications.lock().await;
    let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM communication_sources s JOIN communication_connections c ON c.owner=s.owner WHERE s.id=$1 AND s.version=$2 AND s.enabled AND c.version=$3 AND c.status='active')").bind(source.id).bind(source.version).bind(connection_version).fetch_one(&state.pool).await?;
    if !active {
        return Ok(());
    }
    for (day, messages) in days {
        commit_day(state, source, &day, messages).await?;
    }
    sqlx::query("UPDATE communication_sources SET window_start=$2,window_end=$3,page_token=$4,watermark=CASE WHEN $5 THEN watermark ELSE GREATEST(watermark,$6) END,last_synced_at=CASE WHEN $5 THEN last_synced_at ELSE now() END,next_sync=now()+make_interval(secs=>$7),audit_at=CASE WHEN NOT $5 AND $8 THEN now()+interval '1 day' ELSE audit_at END,error=NULL WHERE id=$1 AND version=$9")
        .bind(source.id).bind(if more {source.window_start} else {None}).bind(if more {source.window_end} else {None}).bind(if more {next} else {""}).bind(more).bind(source.window_end.unwrap_or(source.watermark)).bind(if more {1.0} else {SYNC_SECONDS as f64}).bind(source.window_start==Some(source.start_at)).bind(source.version).execute(&state.pool).await?;
    Ok(())
}
/// 先写新快照、再提交指针、最后回收旧文件；页重放通过 message_id 幂等合并。
async fn commit_day(
    state: &AppState,
    source: &Source,
    day: &str,
    messages: Vec<store::Message>,
) -> ApiResult<()> {
    let sql = format!(
        "SELECT {DOCUMENT_COLUMNS} FROM communication_documents WHERE source_id=$1 AND day=$2"
    );
    let previous: Option<Document> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(source.id)
        .bind(day)
        .fetch_optional(&state.pool)
        .await?;
    let mut document = previous.clone().unwrap_or(Document {
        id: Uuid::new_v4(),
        source_id: source.id,
        day: day.into(),
        raw_hash: String::new(),
        version: 0,
        summary_hash: None,
        summary_error: None,
    });
    let raw = if previous.is_some() {
        store::raw(state, &document)?
    } else {
        vec![]
    };
    let mut merged: BTreeMap<String, store::Message> = raw
        .iter()
        .cloned()
        .map(|m| (m.message_id.clone(), m))
        .collect();
    let mut corrected = false;
    for message in messages {
        if let Some(old) = merged.get(&message.message_id) {
            if old.update_time > message.update_time && !message.deleted {
                continue;
            }
            if old != &message {
                corrected = true;
            }
        }
        merged.insert(message.message_id.clone(), message);
    }
    let mut merged: Vec<_> = merged.into_values().collect();
    merged.sort_by(|a, b| (a.create_time, &a.message_id).cmp(&(b.create_time, &b.message_id)));
    if merged == raw {
        return Ok(());
    }
    // 对旧原文的修正先废止已有回答上下文，不能继续依据撤回的信息推理。
    if corrected {
        dependencies::invalidate_context(state).await?;
    }
    dependencies::cancel(state, Some(document.id), None).await?;
    document.raw_hash = store::write_raw(state, &document, &merged)?;
    document.version += 1;
    document.summary_hash = None;
    sqlx::query("INSERT INTO communication_documents(id,source_id,day,raw_hash,version) VALUES($1,$2,$3,$4,$5) ON CONFLICT(source_id,day) DO UPDATE SET raw_hash=excluded.raw_hash,version=excluded.version,summary_hash=NULL,summary_error=NULL,next_summary=now()")
        .bind(document.id).bind(source.id).bind(day).bind(&document.raw_hash).bind(document.version).execute(&state.pool).await?;
    super::search::remove_vector(state, document.id).await?;
    store::collect(state, &document)?;
    Ok(())
}
/// 仅解析文本和富文本中的文字，附件保留元数据，不伪称已经阅读图片或语音。
fn normalize(value: &Value, chat_id: &str, open_id: &str) -> ApiResult<store::Message> {
    if serde_json::to_vec(value).map_err(unavailable)?.len() > MAX_MESSAGE_BYTES {
        return Err(unavailable("消息过大"));
    }
    let message_id = client::string(value, "message_id")?;
    if value["chat_id"].as_str().is_some_and(|id| id != chat_id) {
        return Err(unavailable("会话不一致"));
    }
    let parse_time = |key: &str| {
        value[key]
            .as_str()
            .and_then(|s| s.parse::<i64>().ok())
            .or_else(|| value[key].as_i64())
    };
    let create_time = parse_time("create_time")
        .filter(|n| *n > 0)
        .ok_or_else(|| unavailable("缺少创建时间"))?;
    let update_time = parse_time("update_time")
        .unwrap_or(create_time)
        .max(create_time);
    let deleted = value["deleted"].as_bool().unwrap_or(false);
    let sender_id = value["sender"]["id"].as_str().unwrap_or("").to_owned();
    let sender_id_type = value["sender"]["id_type"].as_str().unwrap_or("").to_owned();
    let sender_type = value["sender"]["sender_type"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    let message_type = value["msg_type"].as_str().unwrap_or("unknown").to_owned();
    let content = value["body"]["content"].as_str().unwrap_or("");
    let mut text = String::new();
    if !deleted && matches!(message_type.as_str(), "text" | "post") {
        let body: Value = serde_json::from_str(content).map_err(unavailable)?;
        if message_type == "text" {
            text = body["text"]
                .as_str()
                .ok_or_else(|| unavailable("无效文字消息"))?
                .into();
        } else {
            post_text(&body, &mut text);
        }
    }
    Ok(store::Message {
        message_id,
        chat_id: chat_id.into(),
        is_me: sender_id_type == "open_id" && sender_type == "user" && sender_id == open_id,
        sender_id,
        sender_id_type,
        sender_type,
        create_time,
        update_time,
        message_type,
        deleted,
        text,
        payload: if deleted {
            Value::Null
        } else {
            json!({"body":value["body"],"root_id":value["root_id"],"parent_id":value["parent_id"],"thread_id":value["thread_id"]})
        },
    })
}
/// 富文本仅提取标题和文字节点，不把链接目标、图片键或嵌套代码当成动作。
fn post_text(value: &Value, output: &mut String) {
    match value {
        Value::Array(items) => {
            for item in items {
                post_text(item, output)
            }
        }
        Value::Object(fields) => {
            if let Some(text) = fields.get("text").and_then(Value::as_str) {
                output.push_str(text);
                output.push('\n');
            }
            if let Some(title) = fields.get("title").and_then(Value::as_str) {
                output.push_str(title);
                output.push('\n');
            }
            for (key, value) in fields {
                if key != "text" && key != "title" {
                    post_text(value, output);
                }
            }
        }
        _ => {}
    }
}
