use super::{Entry, config::EmbeddingConfig};
use anyhow::{Context, ensure};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

// 限制上游返回体，避免代理错误页或异常向量无限占用内存。
const MAX_RESPONSE_BYTES: usize = 2_000_000;

impl EmbeddingConfig {
    /// 地址、模型、维度、显式修订号共同区分向量空间，不包含密钥。
    pub fn version(&self) -> String {
        crate::auth::hash(&format!(
            "{}|{}|{}|{}",
            self.base_url, self.model, self.dimensions, self.revision
        ))
    }
    /// 输入输出均限长，拒绝维度错误、非有限值与零向量。
    pub async fn embed(&self, client: &reqwest::Client, text: &str) -> anyhow::Result<String> {
        let mut response = client
            .post(format!(
                "{}/embeddings",
                self.base_url.trim_end_matches('/')
            ))
            .bearer_auth(&self.api_key)
            .json(&json!({"model":self.model,"input":text,"encoding_format":"float"}))
            .send()
            .await?
            .error_for_status()?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len() + chunk.len() <= MAX_RESPONSE_BYTES,
                "embedding 响应过大"
            );
            bytes.extend_from_slice(&chunk);
        }
        let body: serde_json::Value = serde_json::from_slice(&bytes)?;
        let values = body["data"][0]["embedding"]
            .as_array()
            .context("缺少 embedding")?;
        ensure!(values.len() == self.dimensions, "embedding 维度不匹配");
        let values: Vec<f32> = values
            .iter()
            .map(|v| v.as_f64().map(|v| v as f32).context("无效向量数值"))
            .collect::<anyhow::Result<_>>()?;
        ensure!(
            values.iter().all(|v| v.is_finite()) && values.iter().any(|v| *v != 0.0),
            "无效向量"
        );
        Ok(serde_json::to_string(&values)?)
    }
}

/// 仅启用语义检索时要求 pgvector，普通迁移和 BM25 模式兼容原有 PostgreSQL。
pub async fn initialize(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::query("CREATE EXTENSION IF NOT EXISTS vector WITH SCHEMA public")
        .execute(pool)
        .await
        .context("请使用支持 pgvector 的 PostgreSQL，并授权创建 vector 扩展")?;
    sqlx::query("CREATE TABLE IF NOT EXISTS memory_vectors (owner TEXT NOT NULL, id UUID NOT NULL, content_hash TEXT NOT NULL, version TEXT NOT NULL, embedding public.vector NOT NULL, PRIMARY KEY(owner,id))")
        .execute(pool).await?;
    Ok(())
}

/// 精确余弦检索适合个人规模；CASE 保证旧模型或不同维度的行不会参与距离计算。
pub async fn search(
    pool: &PgPool,
    config: &EmbeddingConfig,
    owner: &str,
    vector: &str,
) -> anyhow::Result<Vec<(Uuid, String)>> {
    Ok(
        sqlx::query_as(include_str!("../sql/memory_vector_search.sql"))
            .bind(owner)
            .bind(config.version())
            .bind(vector)
            .bind(config.min_similarity)
            .fetch_all(pool)
            .await?,
    )
}

/// 只记录可重建的派生信息，正文仍从经过哈希校验的原文文件读取。
pub async fn save(
    pool: &PgPool,
    config: &EmbeddingConfig,
    owner: &str,
    entry: &Entry,
    vector: &str,
) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO memory_vectors(owner,id,content_hash,version,embedding) VALUES($1,$2,$3,$4,$5::public.vector) ON CONFLICT(owner,id) DO UPDATE SET content_hash=excluded.content_hash,version=excluded.version,embedding=excluded.embedding")
        .bind(owner).bind(entry.id).bind(entry.hash()).bind(config.version()).bind(vector).execute(pool).await?;
    Ok(())
}

/// 即使当前关闭了语义检索，也删除之前启用时留下的向量，避免遗忘残留。
pub async fn remove(pool: &PgPool, owner: &str, id: Uuid) -> anyhow::Result<()> {
    let exists: bool = sqlx::query_scalar("SELECT to_regclass('memory_vectors') IS NOT NULL")
        .fetch_one(pool)
        .await?;
    if exists {
        sqlx::query("DELETE FROM memory_vectors WHERE owner=$1 AND id=$2")
            .bind(owner)
            .bind(id)
            .execute(pool)
            .await?;
    }
    Ok(())
}
