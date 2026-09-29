use anyhow::{Context, ensure};
use std::{env, path::PathBuf};

/// 原文目录与可选语义检索配置；不配置向量时仍提供文件和 BM25。
#[derive(Clone)]
pub struct MemoryConfig {
    /// 必须位于持久化磁盘，不能跨实例共同写入。
    pub directory: PathBuf,
    /// 独立于聊天模型的 embedding 配置。
    pub embedding: Option<EmbeddingConfig>,
    /// 自动从已完成对话抽取明确的长期事实。
    pub auto_extract: bool,
}

/// 一个版本对应唯一向量空间，切换模型、地址或维度会重建索引。
#[derive(Clone)]
pub struct EmbeddingConfig {
    /// 兼容 /embeddings 的 API 根地址。
    pub base_url: String,
    /// 仅服务端使用的授权密钥。
    pub api_key: String,
    /// 不从聊天模型名称推断 embedding 模型。
    pub model: String,
    /// 验证服务商返回的维度，避免混用向量。
    pub dimensions: usize,
    /// 同名模型升级时手动增加此版本。
    pub revision: String,
    /// 查询余弦相似度下限，应按真实语料校准。
    pub min_similarity: f64,
}

impl MemoryConfig {
    /// 没有 embedding 配置也能启动；配置不完整则拒绝静默退回错误模型。
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        if !env::var("MEMORY_ENABLED")
            .unwrap_or("true".into())
            .parse::<bool>()?
        {
            return Ok(None);
        }
        let embedding = match env::var("EMBEDDING_MODEL")
            .ok()
            .filter(|v| !v.trim().is_empty())
        {
            None => None,
            Some(model) => {
                let base_url = env::var("EMBEDDING_BASE_URL").context("缺少 EMBEDDING_BASE_URL")?;
                let url = reqwest::Url::parse(&base_url)?;
                ensure!(
                    matches!(url.scheme(), "http" | "https")
                        && url.host_str().is_some()
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.query().is_none()
                        && url.fragment().is_none(),
                    "embedding 地址无效"
                );
                let dimensions = env::var("EMBEDDING_DIMENSIONS")
                    .context("缺少 EMBEDDING_DIMENSIONS")?
                    .parse()?;
                ensure!(
                    (1..=16000).contains(&dimensions),
                    "embedding 维度范围为 1–16000"
                );
                let min_similarity = env::var("MEMORY_MIN_SIMILARITY")
                    .unwrap_or("0.55".into())
                    .parse()?;
                ensure!(
                    (0.0..=1.0).contains(&min_similarity),
                    "相似度下限必须位于 0–1"
                );
                let api_key = env::var("EMBEDDING_API_KEY").context("缺少 EMBEDDING_API_KEY")?;
                ensure!(!api_key.trim().is_empty(), "EMBEDDING_API_KEY 不能为空");
                Some(EmbeddingConfig {
                    base_url,
                    api_key,
                    model,
                    dimensions,
                    revision: env::var("EMBEDDING_REVISION").unwrap_or("1".into()),
                    min_similarity,
                })
            }
        };
        Ok(Some(Self {
            directory: env::var("MEMORY_DIR").unwrap_or("memory".into()).into(),
            embedding,
            auto_extract: env::var("MEMORY_AUTO_EXTRACT")
                .unwrap_or("true".into())
                .parse()?,
        }))
    }
}
