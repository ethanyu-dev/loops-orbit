use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::HashSet, io::Read, path::Path};

// 限制文件及单次 Jev 判断的规模，避免误填大文件或无界规则。
const MAX_FILE_BYTES: u64 = 65_536;
const MAX_QUESTIONS: usize = 20;
const MAX_QUESTION_CHARS: usize = 500;

/// 文件是问题范围的唯一来源；数据库只保存用于失效检查的快照。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    /// 每项描述一个可接管的问题意图，不填写回复正文。
    questions: Vec<String>,
}

/// 无效文件显式变为空规则，使采集继续但接管保持静默。
pub(super) struct Rules {
    /// 已去除首尾空白并验证的标准问题。
    pub questions: Vec<String>,
    /// 规范化内容的摘要，排版变化不算规则变化。
    pub revision: String,
    /// 仅提供稳定错误码，不暴露文件内容或系统错误。
    pub error: Option<&'static str>,
}
impl Rules {
    /// 每次读取实际文件；不回退到旧快照或内置问题。
    pub fn load(path: &Path) -> Self {
        let parsed = (|| {
            let file = std::fs::File::open(path).map_err(|_| "takeover_rules_unreadable")?;
            let mut bytes = Vec::new();
            file.take(MAX_FILE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "takeover_rules_unreadable")?;
            parse(&bytes)
        })();
        let (questions, error) = match parsed {
            Ok(questions) => (questions, None),
            Err(error) => (vec![], Some(error)),
        };
        let revision = hex::encode(Sha256::digest(
            serde_json::to_vec(&questions).expect("字符串数组可序列化"),
        ));
        Self {
            questions,
            revision,
            error,
        }
    }
}

/// 拒绝空白、重复及超限问题，避免重复规则导致多重命中而一直静默。
fn parse(bytes: &[u8]) -> Result<Vec<String>, &'static str> {
    if bytes.len() > MAX_FILE_BYTES as usize {
        return Err("takeover_rules_invalid");
    }
    let document: Document = serde_json::from_slice(bytes).map_err(|_| "takeover_rules_invalid")?;
    let questions: Vec<String> = document
        .questions
        .into_iter()
        .map(|s| s.trim().to_owned())
        .collect();
    let mut seen = HashSet::new();
    if questions.is_empty()
        || questions.len() > MAX_QUESTIONS
        || questions
            .iter()
            .any(|s| s.is_empty() || s.chars().count() > MAX_QUESTION_CHARS || !seen.insert(s))
    {
        return Err("takeover_rules_invalid");
    }
    Ok(questions)
}

#[cfg(test)]
mod tests {
    use super::*;
    // 验证多问题及格式约束，不代表 Jev 的真实语义识别效果。
    #[test]
    fn validates_multiple_questions_and_rejects_ambiguous_configuration() {
        assert_eq!(
            parse(br#"{"questions":[" first ","second"]}"#).unwrap(),
            vec!["first", "second"]
        );
        for value in [
            serde_json::json!({"questions":[]}),
            serde_json::json!({"questions":[" "]}),
            serde_json::json!({"questions":["same"," same "]}),
            serde_json::json!({"questions":["x"],"unexpected":true}),
            serde_json::json!({"questions":["长".repeat(501)]}),
            serde_json::json!({"questions":(0..21).map(|n|n.to_string()).collect::<Vec<_>>()}),
        ] {
            assert!(parse(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        assert!(parse(b"{broken").is_err());
        assert!(parse(&vec![b' '; MAX_FILE_BYTES as usize + 1]).is_err());
    }
}
