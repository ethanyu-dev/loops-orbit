use serde_json::Value;

// 与中文资料界面一致，优先中文；缺失时只选一个其他语言版本，避免重复拼接译文。
const PREFERRED_LANGUAGES: [&str; 3] = ["zh_cn", "en_us", "ja_jp"];

/// 兼容历史读取接口的直接正文和发送格式的语言包装，只选择一份可见富文本。
pub(super) fn body(value: &Value) -> Option<&Value> {
    let value = value.get("post").unwrap_or(value);
    let is_post = |v: &&Value| v["content"].is_array();
    if is_post(&value) {
        return Some(value);
    }
    PREFERRED_LANGUAGES
        .iter()
        .filter_map(|language| value.get(language))
        .find(is_post)
        .or_else(|| value.as_object()?.values().find(is_post))
}

/// 按段落拼接可见节点，同一行的样式片段不插入换行；不递归读取预览或其他元数据。
pub(super) fn text(value: &Value) -> String {
    let Some(body) = body(value) else {
        return String::new();
    };
    let mut lines = Vec::new();
    if let Some(title) = body["title"].as_str().filter(|title| !title.is_empty()) {
        lines.push(title.to_owned());
    }
    for row in body["content"].as_array().into_iter().flatten() {
        let mut line = String::new();
        for node in row.as_array().into_iter().flatten() {
            if let Some("text" | "a" | "md") = node["tag"].as_str() {
                line.push_str(node["text"].as_str().unwrap_or(""));
            }
        }
        if !line.is_empty() {
            lines.push(line);
        }
    }
    // 不按句子去重：同一正文里的重复段落及相同标题可能是发送者有意保留的内容。
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 覆盖多语言同文与冗余预览，只提取中文正文一次；夹具不代表截图消息的真实原始响应。
    #[test]
    fn selects_one_language_without_preview_metadata() {
        let content = json!({"title":"","content":[[{"tag":"text","text":"上午说的冗余验收 case，误会了"}],[{"tag":"img","image_key":"img_fixture"}]],"preview":{"text":"上午说的冗余验收 case，误会了"}});
        assert_eq!(
            text(&json!({"zh_cn":content,"en_us":content})),
            "上午说的冗余验收 case，误会了"
        );
    }

    /// 覆盖行内格式、链接文案和有意重复段落；不把链接目标或图片键变成正文。
    #[test]
    fn preserves_lines_and_intentional_repetition() {
        assert_eq!(
            text(
                &json!({"title":"确认","content":[[{"tag":"text","text":"请"},{"tag":"a","text":"确认","href":"https://example.com"}],[{"tag":"text","text":"请确认"}],[{"tag":"img","image_key":"img_fixture"}]]})
            ),
            "确认\n请确认\n请确认"
        );
    }

    /// 覆盖单语言回退和 post 包装；不猜测无正文结构的元数据含义。
    #[test]
    fn handles_wrapped_and_fallback_content() {
        assert_eq!(
            text(&json!({"post":{"en_us":{"content":[[{"tag":"text","text":"Hello"}]]}}})),
            "Hello"
        );
        assert_eq!(
            text(&json!({"zh_cn":null,"fr_fr":{"content":[[{"tag":"text","text":"Bonjour"}]]}})),
            "Bonjour"
        );
        assert!(text(&json!({"preview":{"text":"不是正文"}})).is_empty());
    }
}
