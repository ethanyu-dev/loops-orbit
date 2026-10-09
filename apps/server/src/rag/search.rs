use super::{Hit, read_hits, unavailable};
use crate::{AppState, error::ApiResult, memory::lexical};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};
use uuid::Uuid;

// 两路召回分别有界，RRF 仅融合名次，不混用全文分数与余弦分数。
const CANDIDATES: i64 = 32;
const PUBLIC_RESULTS: usize = 6;
const PRIVATE_RESULTS: usize = 8;
const VECTOR_TIMEOUT: Duration = Duration::from_secs(3);
const SQL_TIMEOUT: Duration = Duration::from_secs(2);
const RRF_OFFSET: f64 = 60.0;

/// 对外传 None，只检索已发布正文；本人身份由服务端验证，不能由消息文本声明。
/// 一次问题只生成一个查询向量，同时召回已发布知识和允许的私有分块。
pub(crate) async fn retrieve(
    state: &AppState,
    query: &str,
    owner: Option<&str>,
) -> ApiResult<Vec<Hit>> {
    let started = Instant::now();
    let private = if let Some(owner) = owner {
        crate::auth::is_account_owner(state, owner).await?
            && crate::communications::search::allowed(state, owner).await?
    } else {
        false
    };
    let query: String = query.chars().take(2000).collect();
    let terms: BTreeSet<_> = lexical::tokens(&query).into_iter().take(128).collect();
    if terms.is_empty() {
        return Ok(vec![]);
    }
    // 分词结果只有字母数字；仍使用 SQL 参数，不拼接 SQL 或接收用户查询语法。
    let tsquery = terms
        .into_iter()
        .map(|t| format!("'{t}'"))
        .collect::<Vec<_>>()
        .join(" | ");
    let lexical = async {
        let rows:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM rag_eligible WHERE (scope='published' OR $1) AND lexemes @@ to_tsquery('simple',$2) ORDER BY ts_rank_cd(lexemes,to_tsquery('simple',$2)) DESC,id LIMIT $3")
            .bind(private).bind(tsquery).bind(CANDIDATES).fetch_all(&state.pool).await?;
        Ok::<_, sqlx::Error>(rows)
    };
    let semantic = async {
        let config = state.config.memory.as_ref()?.embedding.as_ref()?;
        // 无索引时不发起 embedding 网络调用，词法检索可立即服务刚发布的内容。
        let available:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM rag_eligible WHERE (scope='published' OR $1) AND embedding_version=$2 AND embedding IS NOT NULL)")
            .bind(private).bind(config.version()).fetch_one(&state.pool).await.ok()?;
        if !available {
            return None;
        }
        let vector = config.embed(&state.http, &query).await.ok()?;
        let rows:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM rag_eligible WHERE (scope='published' OR $1) AND CASE WHEN embedding_version=$2 THEN 1-(embedding OPERATOR(public.<=>) $3::public.vector)>=$4 ELSE false END ORDER BY CASE WHEN embedding_version=$2 THEN embedding OPERATOR(public.<=>) $3::public.vector ELSE NULL END,id LIMIT $5")
            .bind(private).bind(config.version()).bind(vector).bind(config.min_similarity).bind(CANDIDATES).fetch_all(&state.pool).await.ok()?;
        Some(rows)
    };
    let (lexical, semantic) = tokio::join!(
        tokio::time::timeout(SQL_TIMEOUT, lexical),
        tokio::time::timeout(VECTOR_TIMEOUT, semantic)
    );
    let lexical = lexical.map_err(unavailable)?.map_err(unavailable)?;
    let semantic_ok = matches!(semantic, Ok(Some(_)));
    let mut scores: BTreeMap<Uuid, f64> = BTreeMap::new();
    for ranking in [lexical, semantic.ok().flatten().unwrap_or_default()] {
        for (rank, id) in ranking.into_iter().enumerate() {
            *scores.entry(id).or_default() += 1.0 / (RRF_OFFSET + rank as f64);
        }
    }
    let ids: Vec<_> = scores.keys().copied().collect();
    // 在锁内重新投影可见范围，防止网络请求期间的暂停、撤回或解绑穿透校验。
    let _guard = state.communications.lock().await;
    let private = if let Some(owner) = owner {
        private && crate::communications::search::allowed(state, owner).await?
    } else {
        false
    };
    let mut hits = read_hits(state, &ids).await?;
    hits.sort_by(|a, b| {
        scores[&b.id]
            .total_cmp(&scores[&a.id])
            .then_with(|| a.id.cmp(&b.id))
    });
    let (mut public_count, mut private_count) = (0, 0);
    let mut result = vec![];
    // 私有资料只重新读取命中的少量文件核对哈希，绝不每次遍历整个订阅库。
    for hit in hits {
        if hit.scope == "published" {
            if public_count >= PUBLIC_RESULTS {
                continue;
            }
            public_count += 1;
        } else {
            if !private || private_count >= PRIVATE_RESULTS {
                continue;
            }
            let doc: Option<crate::communications::Document> =
                sqlx::query_as(sqlx::AssertSqlSafe(format!(
                    "SELECT {} FROM communication_documents WHERE id=$1",
                    crate::communications::DOCUMENT_COLUMNS
                )))
                .bind(hit.document_id)
                .fetch_optional(&state.pool)
                .await?;
            let Some(doc) = doc else { continue };
            if doc.version != hit.revision
                || format!(
                    "{}:{}",
                    doc.raw_hash,
                    doc.summary_hash.as_deref().unwrap_or_default()
                ) != hit.source_hash
                || crate::communications::store::raw(state, &doc).is_err()
                || (hit.payload["kind"] == "summary"
                    && crate::communications::store::summary(state, &doc).is_err())
            {
                continue;
            }
            private_count += 1;
        }
        result.push(hit);
    }
    tracing::info!(
        duration_ms = started.elapsed().as_millis() as u64,
        public_count,
        private_count,
        semantic_ok,
        "RAG 检索完成"
    );
    Ok(result)
}
