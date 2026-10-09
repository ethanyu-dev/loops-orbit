use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use serde_json::{Value, json};

// 官方 System One 接口，独立于聊天和向量模型。
const DEFAULT_URL: &str = "https://api.typesafe.ai";
const DEFAULT_MODEL: &str = "jev-latest";
const MAX_RESPONSE: usize = 128 * 1024;
const MATCH_PROMPT: &str = include_str!("../../../prompts/takeover_match.md");
const REVIEW_PROMPT: &str = include_str!("../../../prompts/takeover_review.md");

/// Jev 只承担概率判断，凭证不进入浏览器和日志。
#[derive(Clone)]
pub struct Config {
    /// System One API 根地址。
    pub base_url: String,
    /// Typesafe 服务端密钥。
    pub api_key: String,
    /// 控制台可用的模型名。
    pub model: String,
}
impl Config {
    /// 未配置密钥时功能保持不可用，其他聊天和采集不受影响。
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let Some(api_key) = std::env::var("TYPESAFE_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
        else {
            return Ok(None);
        };
        let base_url = std::env::var("TYPESAFE_BASE_URL").unwrap_or(DEFAULT_URL.into());
        let url = reqwest::Url::parse(&base_url)?;
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "TYPESAFE_BASE_URL 无效"
        );
        let model = std::env::var("TYPESAFE_MODEL").unwrap_or(DEFAULT_MODEL.into());
        anyhow::ensure!(!model.trim().is_empty(), "TYPESAFE_MODEL 不能为空");
        Ok(Some(Self {
            base_url,
            api_key,
            model,
        }))
    }
}
/// 单次有界请求；故障直接保持静默，不回退到关键词或其他模型代替 Jev。
async fn ask(state: &AppState, input: Value, questions: Value) -> ApiResult<Value> {
    let config = state.config.typesafe.as_ref().ok_or(error())?;
    let mut response = state
        .http
        .post(format!(
            "{}/v1/systemone",
            config.base_url.trim_end_matches('/')
        ))
        .bearer_auth(&config.api_key)
        .json(&json!({"model":config.model,"state":input,"questions":questions}))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|_| error())?
        .error_for_status()
        .map_err(|_| error())?;
    let mut bytes = vec![];
    while let Some(chunk) = response.chunk().await.map_err(|_| error())? {
        if bytes.len() + chunk.len() > MAX_RESPONSE {
            return Err(error());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| error())
}
/// Noul 是肯定答案的概率，不能将缺失、错误类型或越界值当作置信度。
fn probability(value: &Value, key: &str) -> ApiResult<f64> {
    let answer = &value["answers"][key];
    if answer["type"] != "noul" {
        return Err(error());
    }
    answer["noul"]
        .as_f64()
        .filter(|p| p.is_finite() && (0.0..=1.0).contains(p))
        .ok_or(error())
}
/// 一次请求独立判断所有允许的话题；多个话题都达标时保持静默，避免含糊路由。
pub(super) async fn matching(
    state: &AppState,
    message: &str,
    topics: &[String],
    threshold: f64,
) -> ApiResult<Option<(String, f64)>> {
    let questions: serde_json::Map<String, Value> = topics
        .iter()
        .enumerate()
        .map(|(i, topic)| {
            (
                format!("topic_{i}"),
                json!({"type":"noul","instructions":{"policy":MATCH_PROMPT,"allowed_topic":topic}}),
            )
        })
        .collect();
    let response = ask(
        state,
        json!({"incoming_message":message}),
        Value::Object(questions),
    )
    .await?;
    let mut matches = vec![];
    for (i, topic) in topics.iter().enumerate() {
        let p = probability(&response, &format!("topic_{i}"))?;
        if p >= threshold {
            matches.push((topic.clone(), p));
        }
    }
    Ok(if matches.len() == 1 {
        matches.pop()
    } else {
        None
    })
}
/// 返回真实复核概率供队列持久化；阈值比较由调用方执行，不生成模型未提供的拒绝理由。
pub(super) async fn review(state: &AppState, input: Value) -> ApiResult<f64> {
    let response = ask(
        state,
        input,
        json!({"answerable":{"type":"noul","instructions":REVIEW_PROMPT}}),
    )
    .await?;
    probability(&response, "answerable")
}
/// 对外只暴露稳定错误类别，不携带上游地址、正文或密钥。
fn error() -> ApiError {
    ApiError(StatusCode::BAD_GATEWAY, "takeover_judge_unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;
    // 验证官方 Noul 响应边界；夹具不证明真实 Jev 的中文分类质量或校准程度。
    #[test]
    fn rejects_invalid_probabilities() {
        for answer in [
            json!({}),
            json!({"type":"choice","noul":0.99}),
            json!({"type":"noul","noul":1.1}),
            json!({"type":"noul","noul":"0.95"}),
        ] {
            assert!(probability(&json!({"answers":{"x":answer}}), "x").is_err());
        }
        assert_eq!(
            probability(&json!({"answers":{"x":{"type":"noul","noul":0.95}}}), "x").unwrap(),
            0.95
        );
    }
}
