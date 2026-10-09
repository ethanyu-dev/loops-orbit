use crate::{Failure, failure};
use serde_json::Value;

// 只记录短协议标识，避免把上游意外返回的正文或控制字符写入日志。
const MAX_REASON_CHARS: usize = 64;

/// JSON 和 SSE 共用结束分类与数字用量；usage 缺失表示未知，不能当作零。
#[derive(Default)]
pub(crate) struct Metadata {
    /// 有界的 finish_reason；一旦失败便不允许后续事件覆盖为成功。
    finish_reason: Option<String>,
    /// 上游声明的输入 token 数。
    prompt_tokens: Option<u64>,
    /// 上游声明的输出 token 数，可能包含推理 token。
    completion_tokens: Option<u64>,
    /// 上游单独报告的推理 token 数。
    reasoning_tokens: Option<u64>,
}

impl Metadata {
    /// SSE 的最终 usage 可以位于 choices 为空的独立事件中。
    pub(crate) fn observe(&mut self, data: &Value) {
        if self.validate().is_ok()
            && let Some(reason) = data
                .pointer("/choices/0/finish_reason")
                .and_then(Value::as_str)
        {
            self.finish_reason = Some(
                reason
                    .chars()
                    .take(MAX_REASON_CHARS)
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect(),
            );
        }
        for (target, path) in [
            (&mut self.prompt_tokens, "/usage/prompt_tokens"),
            (&mut self.completion_tokens, "/usage/completion_tokens"),
            (
                &mut self.reasoning_tokens,
                "/usage/completion_tokens_details/reasoning_tokens",
            ),
        ] {
            if let Some(value) = data.pointer(path).and_then(Value::as_u64) {
                *target = Some(value);
            }
        }
    }

    /// 长度截断可在同一模型步骤内恢复；过滤及未知原因不自动重放整个任务。
    pub(crate) fn validate(&self) -> Result<(), Failure> {
        match self.finish_reason.as_deref() {
            None | Some("stop" | "tool_calls") => Ok(()),
            Some("length") => Err(failure("provider_output_limit", false)),
            Some("content_filter") => Err(failure("provider_content_filtered", false)),
            Some(_) => Err(failure("provider_incomplete_response", false)),
        }
    }

    /// 仅输出模型、预算、结束标识及数字统计，不包含密钥、消息、工具参数或推理正文。
    pub(crate) fn log(&self, model: &str, max_tokens: usize) {
        tracing::info!(
            model,
            max_tokens,
            finish_reason = self.finish_reason.as_deref(),
            prompt_tokens = self.prompt_tokens,
            completion_tokens = self.completion_tokens,
            reasoning_tokens = self.reasoning_tokens,
            "模型响应结束信息"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // 验证 usage 独立事件、未知用量及安全标识；不证明上游实际用量统计正确。
    #[test]
    fn metadata_keeps_failure_and_optional_usage() {
        let mut metadata = Metadata::default();
        assert_eq!(metadata.completion_tokens, None);
        metadata.observe(&json!({"choices":[{"finish_reason":"length"}]}));
        metadata.observe(
            &json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":8192,
            "completion_tokens_details":{"reasoning_tokens":8000}}}),
        );
        metadata.observe(&json!({"choices":[{"finish_reason":"stop"}]}));
        assert_eq!(
            metadata.validate().unwrap_err().code,
            "provider_output_limit"
        );
        assert_eq!(metadata.completion_tokens, Some(8192));
        assert_eq!(metadata.reasoning_tokens, Some(8000));
        let mut unknown = Metadata::default();
        unknown.observe(&json!({"choices":[{"finish_reason":"bad\nreason".repeat(20)}]}));
        assert_eq!(
            unknown.finish_reason.as_ref().unwrap().len(),
            MAX_REASON_CHARS
        );
        assert!(!unknown.finish_reason.as_ref().unwrap().contains('\n'));
        assert_eq!(
            unknown.validate().unwrap_err().code,
            "provider_incomplete_response"
        );
    }
}
