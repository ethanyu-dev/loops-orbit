use super::{MAX_ENTRIES, Quote, validate_tags, validate_text};
use crate::{
    AppState, auth, communications,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use chrono::{DateTime, NaiveDate, Timelike, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

// 固定北京时间九点启动前一自然日扫描；不受服务器本地时区影响。
const SCAN_HOUR: u32 = 9;
// 每块独立提交游标和候选，单条超预算明确计数，不伪装为完整处理。
pub(super) const CHUNK_BYTES: usize = 24000;
const MAX_CANDIDATES: usize = 6;
const MAX_ATTEMPTS: i32 = 3;

/// 自动与手动任务均独立于沟通文件；租约和临时输入快照支持重启续跑。
#[derive(sqlx::FromRow)]
struct Job {
    /// 自动与手动任务独立领取，不能只用日期作为定位条件。
    id: Uuid,
    /// 按北京时间定义的待扫描自然日。
    scan_day: NaiveDate,
    /// 首次执行时固定的输入，完成后清空；不保存文件引用。
    messages: Option<Value>,
    /// 下一块从快照中的消息位置继续。
    next_offset: i64,
    /// 当前分块的尝试次数。
    attempts: i32,
    /// 本次执行的随机令牌，阻止旧进程重复提交。
    lease_token: Option<Uuid>,
}

/// 模型只能生成候选内容和证据，没有指定发布权限的字段。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    /// 独立于私人出处的主题。
    title: String,
    /// 需要本人确认的业务知识。
    content: String,
    /// 候选标签随正文一起审批。
    #[serde(default)]
    tags: Vec<String>,
    /// 每项必须逐字命中本批消息。
    evidence: Vec<Quote>,
}

/// 九点之前最多处理前天，九点起允许扫描昨天；日期运算覆盖跨月跨年。
fn due_day(now: DateTime<Utc>) -> NaiveDate {
    let local = now.with_timezone(&chrono_tz::Asia::Shanghai);
    local.date_naive() - chrono::Duration::days(if local.hour() < SCAN_HOUR { 2 } else { 1 })
}

/// 后台入口只在到点后创建每日任务；轮询本身不会触发额外模型调用。
pub async fn step(state: &AppState) -> ApiResult<bool> {
    step_at(state, Utc::now()).await
}

/// 使用可注入时钟验证调度边界；持久日期游标补跑启用后停机错过的扫描。
pub async fn step_at(state: &AppState, now: DateTime<Utc>) -> ApiResult<bool> {
    if state.config.communications.is_none() || state.config.memory.is_none() {
        return Ok(false);
    }
    let mut tx = state.pool.begin().await?;
    let day: Option<NaiveDate> = sqlx::query_scalar("UPDATE knowledge_state SET next_scan_day=next_scan_day+1 WHERE next_scan_day<=$1 RETURNING next_scan_day-1")
        .bind(due_day(now)).fetch_optional(&mut *tx).await?;
    if let Some(day) = day {
        sqlx::query("INSERT INTO knowledge_jobs(id,kind,scope_key,scan_day) VALUES($1,'daily',$2,$3) ON CONFLICT DO NOTHING")
            .bind(Uuid::new_v4())
            .bind(day.to_string())
            .bind(day)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    sqlx::query("UPDATE knowledge_jobs SET status='failed',error='knowledge_extraction_interrupted' WHERE status='running' AND available_at<=now() AND attempts >= $1")
        .bind(MAX_ATTEMPTS).execute(&state.pool).await?;
    let job: Option<Job> = sqlx::query_as("UPDATE knowledge_jobs SET status='running',attempts=attempts+1,lease_token=$1,available_at=now()+interval '150 seconds' WHERE id=(SELECT id FROM knowledge_jobs WHERE status IN ('queued','running') AND available_at<=now() AND attempts<$2 ORDER BY (kind='manual') DESC,available_at,scan_day FOR UPDATE SKIP LOCKED LIMIT 1) RETURNING id,scan_day,messages,next_offset,attempts,lease_token")
        .bind(Uuid::new_v4()).bind(MAX_ATTEMPTS).fetch_optional(&state.pool).await?;
    let Some(job) = job else {
        return Ok(false);
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(120), extract(state, &job))
        .await
        .unwrap_or(Err(ApiError(
            StatusCode::GATEWAY_TIMEOUT,
            "knowledge_extraction_timeout",
        )));
    if let Err(error) = result {
        sqlx::query("UPDATE knowledge_jobs SET status=$3,error=$4,available_at=now()+interval '60 seconds' WHERE id=$1 AND lease_token=$2 AND status='running'")
            .bind(job.id).bind(job.lease_token).bind(if job.attempts>=MAX_ATTEMPTS {"failed"} else {"queued"}).bind(error.1).execute(&state.pool).await?;
        return Err(error);
    }
    Ok(true)
}

/// 扫描所有已启用订阅在指定日的本地原文，只保存独立文本快照及临时编号。
/// 尚未同步到本地的消息不在本次快照内；损坏或未归一化的文件使任务失败，不静默遗漏。
async fn snapshot(state: &AppState, job: &Job) -> ApiResult<Value> {
    if let Some(messages) = &job.messages {
        return Ok(messages.clone());
    }
    let _guard = state.communications.lock().await;
    let migrating: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM communication_sources WHERE enabled AND subscribed AND NOT removal_pending AND day_timezone<>'Asia/Shanghai')")
        .fetch_one(&state.pool).await?;
    if migrating {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "knowledge_calendar_pending",
        ));
    }
    let docs: Vec<communications::Document> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {} FROM communication_documents WHERE day=$1 AND source_id IN(SELECT id FROM communication_sources WHERE enabled AND subscribed AND NOT removal_pending AND day_timezone='Asia/Shanghai') ORDER BY source_id,id", communications::DOCUMENT_COLUMNS)))
        .bind(job.scan_day.to_string()).fetch_all(&state.pool).await?;
    let (snapshot, skipped) = super::snapshot::from_documents(state, &docs).await?;
    sqlx::query("UPDATE knowledge_jobs SET messages=$3,skipped_count=$4 WHERE id=$1 AND lease_token=$2 AND status='running'")
        .bind(job.id).bind(job.lease_token).bind(&snapshot).bind(skipped).execute(&state.pool).await?;
    Ok(snapshot)
}

/// 对自动或手动快照逐块提取并核对证据；知识落库只保留日期及独立摘录，不绑定原文生命周期。
async fn extract(state: &AppState, job: &Job) -> ApiResult<()> {
    let snapshot = snapshot(state, job).await?;
    let messages = snapshot.as_array().ok_or_else(invalid)?;
    let mut chunk = vec![];
    let (mut offset, mut bytes) = (job.next_offset as usize, 0);
    while let Some(message) = messages.get(offset) {
        let size = message.to_string().len();
        if bytes + size > CHUNK_BYTES {
            break;
        }
        bytes += size;
        offset += 1;
        chunk.push(message);
    }
    let candidates: Vec<Candidate> = if chunk.is_empty() {
        vec![]
    } else {
        let output = state
            .runtime
            .extract_knowledge(&json!({"messages":chunk.iter().map(|m|json!({"message_id":m["message_id"],"conversation":m["conversation"],"text":m["text"],"is_me":m["is_me"]})).collect::<Vec<_>>()}))
            .await
            .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "knowledge_extraction_failed"))?;
        serde_json::from_value(output).map_err(|_| invalid())?
    };
    if candidates.len() > MAX_CANDIDATES {
        return Err(invalid());
    }
    for candidate in &candidates {
        validate_text(&candidate.title, &candidate.content)?;
        validate_tags(&candidate.tags)?;
        if candidate.evidence.is_empty()
            || candidate.evidence.len() > 4
            || !candidate.evidence.iter().all(|q| {
                (4..=1000).contains(&q.quote.chars().count())
                    && chunk.iter().any(|m| {
                        m["message_id"] == q.message_id
                            && m["text"].as_str().is_some_and(|s| s.contains(&q.quote))
                    })
            })
        {
            return Err(invalid());
        }
    }
    let _guard = state.communications.lock().await;
    let mut tx = state.pool.begin().await?;
    let active: Option<Uuid> = sqlx::query_scalar("SELECT id FROM knowledge_jobs WHERE id=$1 AND lease_token=$2 AND status='running' FOR UPDATE")
        .bind(job.id).bind(job.lease_token).fetch_optional(&mut *tx).await?;
    if active.is_none() {
        return Ok(());
    }
    let mut count: i64 = sqlx::query_scalar("SELECT count(*) FROM knowledge_entries")
        .fetch_one(&mut *tx)
        .await?;
    let mut created = 0i64;
    for candidate in candidates {
        // 已校验引文关联独立快照；原沟通文件删除不影响审核证据。
        let quotes: Vec<Value> = candidate
            .evidence
            .iter()
            .map(|q| {
                let m=chunk.iter().find(|m|m["message_id"]==q.message_id).expect("证据已校验");
                json!({"quote":q.quote,"snapshot_id":m["snapshot_id"],"source_label":m["source_label"],"source_day":m["source_day"]})
            })
            .collect();
        let fingerprint =
            auth::hash(&json!([candidate.title.trim(), candidate.content.trim()]).to_string());
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM knowledge_entries WHERE extraction_day=$1 AND fingerprint=$2)")
            .bind(job.scan_day).bind(&fingerprint).fetch_one(&mut *tx).await?;
        if exists {
            continue;
        }
        if count >= MAX_ENTRIES {
            return Err(ApiError(StatusCode::CONFLICT, "knowledge_limit"));
        }
        sqlx::query("INSERT INTO knowledge_entries(id,title,content,extraction_day,evidence,fingerprint,tags) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(Uuid::new_v4()).bind(candidate.title.trim()).bind(candidate.content.trim()).bind(job.scan_day).bind(json!(quotes)).bind(fingerprint).bind(candidate.tags).execute(&mut *tx).await?;
        count += 1;
        created += 1;
    }
    let completed = offset >= messages.len();
    sqlx::query("UPDATE knowledge_jobs SET next_offset=$3,status=$4,created_count=created_count+$6,attempts=0,lease_token=NULL,available_at=now(),error=NULL,messages=CASE WHEN $5 THEN NULL ELSE messages END WHERE id=$1 AND lease_token=$2")
        .bind(job.id).bind(job.lease_token).bind(offset as i64).bind(if completed {"completed"} else {"queued"}).bind(completed).bind(created).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// 固定错误码不包含原文或未经验证的模型输出。
fn invalid() -> ApiError {
    ApiError(StatusCode::BAD_GATEWAY, "knowledge_evidence_invalid")
}

#[cfg(test)]
mod tests {
    use super::*;

    // 验证北京时间九点前后及跨年日期；不验证后台实际唤醒延迟或上游同步完整性。
    #[test]
    fn schedule_uses_beijing_nine_am() {
        for (time, day) in [
            ("2026-10-09T00:59:59Z", "2026-10-07"),
            ("2026-10-09T01:00:00Z", "2026-10-08"),
            ("2027-01-01T01:00:00Z", "2026-12-31"),
        ] {
            let now = DateTime::parse_from_rfc3339(time)
                .unwrap()
                .with_timezone(&Utc);
            assert_eq!(due_day(now).to_string(), day);
        }
    }
}
