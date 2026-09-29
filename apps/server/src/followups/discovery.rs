use super::{Create, Source, create, preferences};
use crate::AppState;
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::watch;
use uuid::Uuid;

// 默认两天后回访；用户在这期间的补充会在真正发送前重新评估。
const DELAY_DAYS: i64 = 2;
/// 自动发现只提出一个有用户原文依据的具体事项。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    /// 简短具体的未完事项。
    topic: String,
    /// 必须来自该完成批次用户原文的逐字片段。
    evidence: String,
}
/// 发现慢请求独立于到期调度，失败按持久化记录有界重试。
pub async fn run(state: AppState, mut stop: watch::Receiver<bool>) {
    loop {
        if *stop.borrow() {
            break;
        }
        if process_one(&state).await.is_err() {
            tracing::warn!("发现回访事项失败，等待重试");
        }
        tokio::select! {_=tokio::time::sleep(std::time::Duration::from_secs(3))=>{},_=stop.changed()=>{}}
    }
}
/// 只消费用户已开启回访后的完成轮次，不回填所有历史。
pub async fn process_one(state: &AppState) -> anyhow::Result<bool> {
    let job: Option<(Uuid, String, i32)> =
        sqlx::query_as(include_str!("../sql/followup_discovery_claim.sql"))
            .fetch_optional(&state.pool)
            .await?;
    let Some((id, owner, attempt)) = job else {
        return Ok(false);
    };
    let result = if attempt <= 3 {
        discover(state, id, &owner, attempt).await
    } else {
        Err(anyhow::anyhow!("发现尝试超限"))
    };
    let status = if result.is_ok() {
        "completed"
    } else if attempt >= 3 {
        "failed"
    } else {
        "queued"
    };
    sqlx::query("UPDATE followup_discovery SET status=$3,available_at=now()+interval '1 minute' WHERE run_id=$1 AND attempts=$2 AND status='queued'")
        .bind(id).bind(attempt).bind(status).execute(&state.pool).await?;
    result?;
    Ok(true)
}
/// 模型判断后重验原文、开关、来源状态与遗忘边界，再创建持久化事项。
async fn discover(state: &AppState, id: Uuid, owner: &str, attempt: i32) -> anyhow::Result<()> {
    if !preferences(state, owner)
        .await
        .map_err(|_| anyhow::anyhow!("读取偏好失败"))?
        .enabled
    {
        return Ok(());
    }
    let boundary: i64 = sqlx::query_scalar(
        "SELECT COALESCE((SELECT forgotten_through FROM memory_owners WHERE owner=$1),0)",
    )
    .bind(owner)
    .fetch_one(&state.pool)
    .await?;
    let inputs: Vec<String> = sqlx::query_scalar(include_str!("../sql/memory_sources.sql"))
        .bind(id)
        .bind(boundary)
        .fetch_all(&state.pool)
        .await?;
    if inputs.is_empty() {
        return Ok(());
    }
    let existing:Vec<String>=sqlx::query_scalar("SELECT topic FROM followups WHERE owner=$1 AND status NOT IN('cancelled','expired','failed') ORDER BY updated_at DESC LIMIT 50")
        .bind(owner).fetch_all(&state.pool).await?;
    let output = state
        .runtime
        .followup_decision(
            &json!({"now":Utc::now(),"user_messages":inputs,"existing":existing}),
            true,
        )
        .await?;
    if output == Value::Null {
        return Ok(());
    }
    let candidate: Candidate = serde_json::from_value(output)?;
    anyhow::ensure!(
        candidate.evidence.chars().count() >= 4
            && inputs.iter().any(|s| s.contains(&candidate.evidence)),
        "回访缺少原文依据"
    );
    if existing.iter().any(|topic| topic == &candidate.topic) {
        return Ok(());
    }
    let job:crate::worker::Job=sqlx::query_as("SELECT id,conversation_id,seq,batch_id,input,attempts,lease_token,reply_to FROM runs WHERE id=$1 AND status='completed'")
        .bind(id).fetch_one(&state.pool).await?;
    let memory_ids = if state.config.memory.is_some() {
        crate::memory::search(state, owner, &candidate.topic)
            .await
            .map_err(|_| anyhow::anyhow!("读取依赖失败"))?
            .into_iter()
            .map(|e| e.id)
            .collect()
    } else {
        vec![]
    };
    let input = Create {
        communication: None,
        idempotency_key: id,
        conversation_id: Some(job.conversation_id),
        kind: "checkin".into(),
        topic: candidate.topic,
        due_at: (Utc::now() + Duration::days(DELAY_DAYS)).to_rfc3339(),
        expires_at: None,
        memory_ids,
    };
    let source = Source {
        job: &job,
        operation_key: "discovery".into(),
        discovery_attempt: Some(attempt),
    };
    match create(state, owner, &input, Some(&source)).await {
        Ok(_) => Ok(()),
        Err(error)
            if matches!(
                error.1,
                "checkins_disabled" | "run_superseded" | "followup_owner_unavailable"
            ) =>
        {
            Ok(())
        }
        Err(_) => Err(anyhow::anyhow!("创建回访失败")),
    }
}
