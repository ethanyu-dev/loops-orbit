pub(crate) mod boundary;
pub mod config;
pub(crate) mod embedding;
pub(crate) mod lexical;
pub mod routes;
pub(crate) mod store;
pub mod worker;

use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use anyhow::ensure;
use axum::http::StatusCode;
use boundary::{invalidate, sync_boundary};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;
use uuid::Uuid;

// 单条记忆和每身份总量有上限；常驻资料与检索资料分别分配预算。
const CONTEXT_PROMPT: &str = include_str!("../../prompts/memory_context.md");
const MAX_ENTRY_CHARS: usize = 2000;
const MAX_ENTRIES: usize = 1000;
const PROFILE_CHARS: usize = 4000;
const CONTEXT_CHARS: usize = 9000;
const RRF_OFFSET: f64 = 60.0;
// 限制闲置访客索引的常驻数量，清空后仍可从文件重建。
const CACHED_OWNERS: usize = 16;
// 首字延迟不能被向量供应商故障拖到完整 HTTP 超时。
const QUERY_TIMEOUT_SECONDS: u64 = 3;

/// 文件中的原子事实，同一 key 表示可被新信息修正的同一主题。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// 服务端产生的稳定标识，与文件名一致。
    pub id: Uuid,
    /// profile / project / episode，控制常驻还是按需召回。
    pub kind: String,
    /// 如 reply.style 或 project.orbit.storage，更新沿用相同键。
    pub key: String,
    /// 人类可读的事实正文，不接受系统指令。
    pub content: String,
    /// 真实文件更新时刻，不由模型伪造。
    pub updated_at: DateTime<Utc>,
    /// 临时事项的到期时间；过期信息不会送给模型。
    pub expires_at: Option<DateTime<Utc>>,
    /// 自动抽取的来源任务；手工输入为空。
    pub source_run: Option<Uuid>,
    /// 来源单调序号，防止较旧后台任务覆盖新事实。
    pub source_seq: i64,
    /// 删除保留无正文的墓碑，旧任务不能恢复同一文件。
    pub deleted: bool,
}
impl Entry {
    /// 文件手改与 API 输入使用相同校验，不允许无界文档进入提示词。
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            matches!(self.kind.as_str(), "profile" | "project" | "episode"),
            "无效记忆类型"
        );
        ensure!(
            !self.key.trim().is_empty() && self.key.chars().count() <= 120,
            "无效记忆主题"
        );
        ensure!(
            !self.content.trim().is_empty() && self.content.chars().count() <= MAX_ENTRY_CHARS,
            "无效记忆正文"
        );
        Ok(())
    }
    /// 到期和墓碑永不参与任一路召回。
    pub fn active(&self) -> bool {
        !self.deleted && self.expires_at.is_none_or(|at| at > Utc::now())
    }
    /// 索引版本覆盖类型、主题、正文和元数据，修正来源也触发重建。
    pub fn hash(&self) -> String {
        crate::auth::hash(&serde_json::to_string(self).expect("记忆可序列化"))
    }
}

/// 单实例写入器与可丢弃的分词索引缓存；所有写入和遗忘共用此锁。
#[derive(Default)]
pub struct MemoryState {
    /// 每身份独立构建，绝不先全局召回再过滤身份。
    lexical: HashMap<String, Arc<lexical::Lexical>>,
}
/// 应用状态共享一个写锁，网络调用在锁外完成并在落盘前再次校验。
pub type SharedMemory = Arc<Mutex<MemoryState>>;

/// 错误不包含记忆正文、路径或服务商凭据。
fn unavailable(_: impl std::fmt::Display) -> ApiError {
    tracing::warn!("记忆存储或索引暂时不可用");
    ApiError(StatusCode::SERVICE_UNAVAILABLE, "memory_unavailable")
}

/// 文件锁同时验证目录可写，并防止不同数据库配置误共享同一原文目录。
/// 返回句柄必须由主进程保留至全部写入任务退出。
pub fn lock_directory(config: &config::MemoryConfig) -> anyhow::Result<std::fs::File> {
    std::fs::create_dir_all(&config.directory)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(config.directory.join(".writer.lock"))?;
    file.try_lock()?;
    Ok(file)
}

/// 启动时重建数据库遗忘边界，向量扩展只在配置了 embedding 时检查。
pub async fn initialize(state: &AppState) -> anyhow::Result<()> {
    let Some(config) = &state.config.memory else {
        return Ok(());
    };
    std::fs::create_dir_all(&config.directory)?;
    if config.embedding.is_some() {
        embedding::initialize(&state.pool).await?;
    }
    let owners: Vec<String> =
        sqlx::query_scalar("SELECT owner FROM memory_owners UNION SELECT owner FROM conversations")
            .fetch_all(&state.pool)
            .await?;
    for owner in owners {
        sync_boundary(state, &owner)
            .await
            .map_err(|_| anyhow::anyhow!("恢复记忆边界失败"))?;
    }
    Ok(())
}

/// 读取原文时先恢复边界，避免运行过程中上次部分成功的写入失去保护。
pub async fn list(state: &AppState, owner: &str) -> ApiResult<Vec<Entry>> {
    let _guard = state.memory.lock().await;
    sync_boundary(state, owner).await?;
    let config = state.config.memory.as_ref().expect("已验证启用");
    store::read(&config.directory, owner).map_err(unavailable)
}

/// 人工写入显式覆盖同一主题；自动提取走独立的来源与版本校验。
pub async fn save(state: &AppState, owner: &str, mut entry: Entry, edit: bool) -> ApiResult<Entry> {
    entry.content = entry.content.trim().to_owned();
    entry.key = entry.key.trim().to_owned();
    entry
        .validate()
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_memory"))?;
    let mut guard = state.memory.lock().await;
    sync_boundary(state, owner).await?;
    let config = state.config.memory.as_ref().expect("已验证启用");
    let entries = store::read(&config.directory, owner).map_err(unavailable)?;
    if edit && !entries.iter().any(|e| e.id == entry.id && !e.deleted) {
        return Err(ApiError(StatusCode::NOT_FOUND, "memory_not_found"));
    }
    if entries
        .iter()
        .any(|e| !e.deleted && e.id != entry.id && e.key == entry.key)
    {
        return Err(ApiError(StatusCode::CONFLICT, "memory_key_exists"));
    }
    if !edit && entries.iter().filter(|e| e.active()).count() >= MAX_ENTRIES {
        return Err(ApiError(StatusCode::CONFLICT, "memory_limit"));
    }
    // 人工新增也推进边界，防止先前在途提取任务覆盖用户的明确决定。
    invalidate(state, owner).await?;
    if let Some(old) = entries.iter().find(|old| old.id == entry.id) {
        crate::followups::service_memory_changed(state, owner, old.id, old.source_run).await?;
    }
    entry.source_seq = store::boundary(&config.directory, owner).map_err(unavailable)?;
    store::write(&config.directory, owner, &entry).map_err(unavailable)?;
    guard.lexical.remove(owner);
    embedding::remove(&state.pool, owner, entry.id)
        .await
        .map_err(unavailable)?;
    Ok(entry)
}

/// 墓碑删除正文，旧索引即使尚未清理也因哈希不匹配而无法返回。
pub async fn forget(state: &AppState, owner: &str, id: Uuid) -> ApiResult<()> {
    let mut guard = state.memory.lock().await;
    sync_boundary(state, owner).await?;
    let config = state.config.memory.as_ref().expect("已验证启用");
    let mut entry = store::read(&config.directory, owner)
        .map_err(unavailable)?
        .into_iter()
        .find(|e| e.id == id && !e.deleted)
        .ok_or(ApiError(StatusCode::NOT_FOUND, "memory_not_found"))?;
    invalidate(state, owner).await?;
    crate::followups::service_memory_changed(state, owner, id, entry.source_run).await?;
    entry.deleted = true;
    entry.content.clear();
    entry.key.clear();
    entry.source_run = None;
    entry.updated_at = Utc::now();
    store::write(&config.directory, owner, &entry).map_err(unavailable)?;
    guard.lexical.remove(owner);
    embedding::remove(&state.pool, owner, id)
        .await
        .map_err(unavailable)?;
    Ok(())
}

/// BM25 与语义各取候选，按 RRF 融合；任何旧版本必须回原文校验才能命中。
pub async fn search(state: &AppState, owner: &str, query: &str) -> ApiResult<Vec<Entry>> {
    let entries: Vec<Entry> = list(state, owner)
        .await?
        .into_iter()
        .filter(Entry::active)
        .collect();
    if entries.is_empty() || query.trim().is_empty() {
        return Ok(vec![]);
    }
    let fingerprint = crate::auth::hash(&entries.iter().map(Entry::hash).collect::<String>());
    let lexical = {
        let mut guard = state.memory.lock().await;
        if !guard
            .lexical
            .get(owner)
            .is_some_and(|l| l.fingerprint == fingerprint)
        {
            let documents = entries.clone();
            let index = tokio::task::spawn_blocking(move || {
                lexical::Lexical::build(&documents, fingerprint)
            })
            .await
            .map_err(unavailable)?
            .map_err(unavailable)?;
            if guard.lexical.len() >= CACHED_OWNERS {
                guard.lexical.clear();
            }
            guard.lexical.insert(owner.into(), Arc::new(index));
        }
        guard.lexical[owner].clone()
    };
    let query_text = query.chars().take(2000).collect::<String>();
    let mut lists = vec![lexical.search(&query_text).map_err(unavailable)?];
    if let Some(config) = &state.config.memory.as_ref().expect("已验证启用").embedding {
        let semantic = async {
            let vector = config.embed(&state.http, &query_text).await?;
            embedding::search(&state.pool, config, owner, &vector).await
        };
        match tokio::time::timeout(
            std::time::Duration::from_secs(QUERY_TIMEOUT_SECONDS),
            semantic,
        )
        .await
        {
            Ok(Ok(hits)) => lists.push(
                hits.into_iter()
                    .filter(|(id, hash)| entries.iter().any(|e| e.id == *id && e.hash() == *hash))
                    .map(|(id, _)| id)
                    .collect(),
            ),
            _ => tracing::warn!("语义召回失败或超时，本轮使用 BM25"),
        }
    }
    let mut ranks: HashMap<Uuid, f64> = HashMap::new();
    for hits in lists {
        for (rank, id) in hits.into_iter().enumerate() {
            *ranks.entry(id).or_default() += 1.0 / (RRF_OFFSET + rank as f64 + 1.0);
        }
    }
    let mut ranks: Vec<_> = ranks.into_iter().collect();
    ranks.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    // 网络请求期间可能被删除或修正，返回前必须再次读取原文并验证指纹。
    let current = list(state, owner).await?;
    Ok(ranks
        .into_iter()
        .filter_map(|(id, _)| {
            entries
                .iter()
                .find(|e| e.id == id)
                .filter(|old| {
                    current
                        .iter()
                        .any(|new| new.active() && new.id == id && new.hash() == old.hash())
                })
                .cloned()
        })
        .take(6)
        .collect())
}

/// 小型档案每轮载入，项目与事件根据当前请求及近期指代补充查询。
pub async fn context(
    state: &AppState,
    owner: &str,
    history: &[agent_runtime::Message],
) -> ApiResult<String> {
    if state.config.memory.is_none() {
        return Ok(String::new());
    }
    let entries = list(state, owner).await?;
    let mut selected = Vec::new();
    let mut size = 0;
    for entry in entries.iter().filter(|e| e.active() && e.kind == "profile") {
        let length = entry.content.chars().count() + entry.key.chars().count() + 100;
        if size + length > PROFILE_CHARS {
            continue;
        }
        size += length;
        selected.push(entry.clone());
    }
    let query = history
        .iter()
        .rev()
        .filter(|m| m.role == "user")
        .take(2)
        .map(|m| m.content.chars().take(1000).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    for entry in search(state, owner, &query).await? {
        if selected.iter().any(|e| e.id == entry.id) {
            continue;
        }
        let length = entry.content.chars().count() + entry.key.chars().count() + 100;
        if size + length > CONTEXT_CHARS {
            break;
        }
        size += length;
        selected.push(entry);
    }
    let capability = if state.config.memory.as_ref().is_some_and(|c| c.auto_extract) {
        "回复完成后会后台提取明确的稳定事实；当前回复时尚未抽取，保存结果以记忆页面为准。"
    } else {
        "本服务仅支持在记忆页面手工保存，后台自动提取已关闭。"
    };
    Ok(CONTEXT_PROMPT
        .replace("{{capability}}", capability)
        .replace(
            "{{entries}}",
            &serde_json::to_string(&selected).map_err(unavailable)?,
        ))
}
