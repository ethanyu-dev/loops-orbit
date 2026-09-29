use super::{Entry, MAX_ENTRIES, embedding, store, sync_boundary};
use crate::AppState;
use anyhow::ensure;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use sqlx::FromRow;
use std::time::Duration;
use tokio::sync::watch;
use uuid::Uuid;

// 提取请求最多重试三次，每次租约比模型超时长；索引每轮有工作量上限。
const MAX_ATTEMPTS: i32 = 3;
const INDEX_BATCH: usize = 16;
const POLL_SECONDS: u64 = 3;
// 已有事实只用于去重与修正，不把整个个人档案无限填进抽取请求。
const CATALOG_CHARS: usize = 12_000;

/// 队列只引用已完成的任务，不复制聊天正文。
#[derive(FromRow)]
struct Job {
    /// 对应已完成回复。
    run_id: Uuid,
    /// 由原会话解析的身份，不由模型决定。
    owner: String,
    /// 用于遗忘与乱序保护的全局单调序号。
    source_seq: i64,
    /// 同时作为本次处理的围栏版本。
    attempts: i32,
}
/// 模型输出在进入原文目录前需校验结构、证据和主题。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    /// 对应现有主题或新事实。
    key: String,
    /// 决定常驻或按需召回。
    kind: String,
    /// 精简事实，不能替代来源证据。
    content: String,
    /// 必须是本批用户原文的连续片段。
    evidence: String,
    /// 临时事实的明确失效时间。
    expires_at: Option<DateTime<Utc>>,
}

/// 单独后台循环，抽取或 embedding 失败不会把已经完成的聊天改为失败。
pub async fn run(state: AppState, mut stop: watch::Receiver<bool>) {
    if state.config.memory.is_none() {
        let _ = stop.changed().await;
        return;
    }
    loop {
        if *stop.borrow() {
            break;
        }
        if extract_one(&state).await.is_err() {
            tracing::warn!("后台记忆抽取失败，将按队列策略重试");
        }
        if *stop.borrow() {
            break;
        }
        if reconcile(&state).await.is_err() {
            tracing::warn!("记忆向量补建失败，将在后续周期重试");
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(POLL_SECONDS)) => {},
            _ = stop.changed() => {},
        }
    }
}

/// 单条原子领取并延期，崩溃后可恢复，旧尝试不能标记或写入新尝试的结果。
pub async fn extract_one(state: &AppState) -> anyhow::Result<bool> {
    if !state.config.memory.as_ref().is_some_and(|c| c.auto_extract) {
        return Ok(false);
    }
    let job: Option<Job> = sqlx::query_as(include_str!("../sql/memory_claim.sql"))
        .fetch_optional(&state.pool)
        .await?;
    let Some(job) = job else {
        return Ok(false);
    };
    let result = if job.attempts <= MAX_ATTEMPTS {
        tokio::time::timeout(Duration::from_secs(115), extract(state, &job))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r)
    } else {
        Err(anyhow::anyhow!("记忆任务尝试上限"))
    };
    let status = if result.is_ok() {
        "completed"
    } else if job.attempts >= MAX_ATTEMPTS {
        "failed"
    } else {
        "queued"
    };
    sqlx::query("UPDATE memory_jobs SET status=$3,available_at=now()+interval '30 seconds' WHERE run_id=$1 AND attempts=$2 AND status='queued'")
        .bind(job.run_id).bind(job.attempts).bind(status).execute(&state.pool).await?;
    result?;
    Ok(true)
}

/// 模型请求在锁外；写入时重新检查遗忘边界、来源状态、租约及原文版本。
async fn extract(state: &AppState, job: &Job) -> anyhow::Result<()> {
    let config = state.config.memory.as_ref().expect("已验证启用");
    let existing = super::list(state, &job.owner)
        .await
        .map_err(|_| anyhow::anyhow!("读取记忆失败"))?;
    if job.source_seq <= store::boundary(&config.directory, &job.owner)? {
        return Ok(());
    }
    let inputs: Vec<String> = sqlx::query_scalar(include_str!("../sql/memory_sources.sql"))
        .bind(job.run_id)
        .bind(store::boundary(&config.directory, &job.owner)?)
        .fetch_all(&state.pool)
        .await?;
    if inputs.is_empty() {
        return Ok(());
    }
    let mut catalog_chars = 0;
    let catalog: Vec<_> = existing
        .iter()
        .filter(|e| e.active())
        .take_while(|e| {
            catalog_chars += e.content.chars().count() + e.key.chars().count();
            catalog_chars <= CATALOG_CHARS
        })
        .map(|e| json!({"key":e.key,"kind":e.kind,"content":e.content}))
        .take(80)
        .collect();
    let output = state
        .runtime
        .extract_memory(&json!({"now":Utc::now(),"user_messages":inputs,"existing":catalog}))
        .await?;
    let candidates: Vec<Candidate> = serde_json::from_value(output)?;
    ensure!(candidates.len() <= 4, "提取数量超限");
    let mut keys = std::collections::HashSet::new();
    // 整批先验证，避免后半条损坏时留下部分不可解释的写入。
    for candidate in &candidates {
        ensure!(
            candidate.evidence.chars().count() >= 4
                && inputs
                    .iter()
                    .any(|input| input.contains(&candidate.evidence)),
            "提取缺少用户原文证据"
        );
        ensure!(keys.insert(candidate.key.clone()), "同批重复主题");
        Entry {
            id: Uuid::new_v4(),
            kind: candidate.kind.clone(),
            key: candidate.key.clone(),
            content: candidate.content.clone(),
            updated_at: Utc::now(),
            expires_at: candidate.expires_at,
            source_run: Some(job.run_id),
            source_seq: job.source_seq,
            deleted: false,
        }
        .validate()?;
    }
    let mut guard = state.memory.lock().await;
    sync_boundary(state, &job.owner)
        .await
        .map_err(|_| anyhow::anyhow!("同步遗忘边界失败"))?;
    if job.source_seq <= store::boundary(&config.directory, &job.owner)? {
        return Ok(());
    }
    let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memory_jobs j JOIN runs r ON r.id=j.run_id WHERE j.run_id=$1 AND j.attempts=$2 AND j.status='queued' AND r.status='completed')")
        .bind(job.run_id).bind(job.attempts).fetch_one(&state.pool).await?;
    if !active {
        return Ok(());
    }
    let current = store::read(&config.directory, &job.owner)?;
    let mut count = current.iter().filter(|e| e.active()).count();
    for candidate in candidates {
        let old = current
            .iter()
            .find(|e| !e.deleted && e.key == candidate.key);
        if let Some(old) = old {
            if old.source_seq >= job.source_seq
                || !existing
                    .iter()
                    .any(|e| e.id == old.id && e.hash() == old.hash())
            {
                continue;
            }
        } else {
            if count >= MAX_ENTRIES {
                continue;
            }
            count += 1;
        }
        let entry = Entry {
            id: old.map_or_else(Uuid::new_v4, |e| e.id),
            key: candidate.key,
            kind: candidate.kind,
            content: candidate.content,
            updated_at: Utc::now(),
            expires_at: candidate.expires_at,
            source_run: Some(job.run_id),
            source_seq: job.source_seq,
            deleted: false,
        };
        if let Some(old) = old {
            crate::followups::service_memory_changed(state, &job.owner, old.id, old.source_run)
                .await
                .map_err(|_| anyhow::anyhow!("取消旧记忆跟进失败"))?;
        }
        store::write(&config.directory, &job.owner, &entry)?;
    }
    guard.lexical.remove(&job.owner);
    Ok(())
}

/// 周期扫描文件哈希，修正、删除、到期、模型切换均可从原文恢复正确向量。
pub async fn reconcile(state: &AppState) -> anyhow::Result<()> {
    let Some(config) = &state.config.memory else {
        return Ok(());
    };
    let Some(embedding_config) = &config.embedding else {
        return Ok(());
    };
    let owners: Vec<String> =
        sqlx::query_scalar("SELECT owner FROM memory_owners UNION SELECT owner FROM conversations")
            .fetch_all(&state.pool)
            .await?;
    let mut built = 0;
    for owner in owners {
        let entries = super::list(state, &owner)
            .await
            .map_err(|_| anyhow::anyhow!("读取记忆失败"))?;
        let existing: Vec<(Uuid, String, String)> =
            sqlx::query_as("SELECT id,content_hash,version FROM memory_vectors WHERE owner=$1")
                .bind(&owner)
                .fetch_all(&state.pool)
                .await?;
        for (id, hash, version) in &existing {
            if !entries.iter().any(|entry| {
                entry.active()
                    && entry.id == *id
                    && entry.hash() == *hash
                    && *version == embedding_config.version()
            }) {
                sqlx::query("DELETE FROM memory_vectors WHERE owner=$1 AND id=$2 AND content_hash=$3 AND version=$4").bind(&owner).bind(id).bind(hash).bind(version).execute(&state.pool).await?;
            }
        }
        for entry in entries.iter().filter(|e| e.active()) {
            if existing.iter().any(|(id, hash, version)| {
                *id == entry.id && *hash == entry.hash() && *version == embedding_config.version()
            }) {
                continue;
            }
            if built >= INDEX_BATCH {
                return Ok(());
            }
            built += 1;
            let vector = embedding_config
                .embed(&state.http, &format!("{}\n{}", entry.key, entry.content))
                .await?;
            let _guard = state.memory.lock().await;
            let current = store::read(&config.directory, &owner)?;
            if current
                .iter()
                .any(|e| e.active() && e.id == entry.id && e.hash() == entry.hash())
            {
                embedding::save(&state.pool, embedding_config, &owner, entry, &vector).await?;
            }
        }
    }
    Ok(())
}
