use serde_json::Value;

/// 依据上游明确的 ID 映射还原提及；当前授权账号展示为“我”，不使用昵称判定本人。
pub(super) fn resolve(text: &str, mentions: &Value, owner: &str) -> String {
    let mut mappings: Vec<(&str, String)> = mentions
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|mention| {
            let key = mention["key"].as_str()?;
            if !key.starts_with("@_user_")
                || !key[7..].bytes().all(|b| b.is_ascii_digit())
                || key.len() <= 7
            {
                return None;
            }
            let is_me = (mention["id_type"] == "open_id" && mention["id"] == owner)
                || mention["id"]["open_id"] == owner;
            let name = if is_me {
                "我"
            } else if mention["id"] == "all" {
                "所有人"
            } else {
                mention["name"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("成员（姓名未获取）")
            };
            Some((
                key,
                format!("@{}", name.trim().chars().take(120).collect::<String>()),
            ))
        })
        .collect();
    mappings.sort_by_key(|(key, _)| std::cmp::Reverse(key.len()));
    let mut output = String::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        // 逐次扫描原始输入，防止短占位符误替换长编号，或姓名中的占位符被二次替换。
        if let Some((key, name)) = mappings.iter().find(|(key, _)| {
            remaining.starts_with(key)
                && remaining[key.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
        }) {
            output.push_str(name);
            remaining = &remaining[key.len()..];
        } else {
            let character = remaining.chars().next().expect("输入非空");
            output.push(character);
            remaining = &remaining[character.len_utf8()..];
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    /// 覆盖真实占位符形状、本人标记及编号前缀碰撞；不验证租户姓名接口权限。
    #[test]
    fn resolves_names_without_prefix_collisions() {
        let mentions = json!([{"key":"@_user_1","id":"ou_other","id_type":"open_id","name":"小林"},{"key":"@_user_10","id":"ou_me","id_type":"open_id","name":"本人姓名"}]);
        assert_eq!(
            resolve("@_user_1 @_user_10 请确认 @_user_100", &mentions, "ou_me"),
            "@小林 @我 请确认 @_user_100"
        );
    }
    /// 姓名作为展示数据只插入一次，不能触发其他映射或伪造“我”。
    #[test]
    fn replacements_are_not_reprocessed() {
        let mentions = json!([{"key":"@_user_1","id":"ou_other","id_type":"user_id","name":"_user_2"},{"key":"@_user_2","id":"ou_me","id_type":"open_id","name":"本人"}]);
        assert_eq!(
            resolve("@_user_1 @_user_2", &mentions, "ou_me"),
            "@_user_2 @我"
        );
    }
}
