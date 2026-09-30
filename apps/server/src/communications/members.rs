use super::{Source, client};
use crate::AppState;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

// 每个消息页最多查询 20 页成员，避免异常游标或超大群无限阻塞采集；缺失名称不猜测。
const MAX_MEMBER_PAGES: usize = 20;

/// 只为当前页缺名的真人查询成员列表；纯机器人消息不调用真人成员接口。
pub(super) async fn names(
    state: &AppState,
    source: &Source,
    token: &str,
    items: &[Value],
    owner: &str,
) -> BTreeMap<String, String> {
    let mut missing: BTreeSet<String> = items
        .iter()
        .filter(|item| {
            item["sender"]["sender_type"] == "user"
                && item["sender"]["id_type"] == "open_id"
                && item["sender"]["name"]
                    .as_str()
                    .unwrap_or("")
                    .trim()
                    .is_empty()
        })
        .filter_map(|item| item["sender"]["id"].as_str())
        .filter(|id| *id != owner)
        .map(str::to_owned)
        .collect();
    let mut names = BTreeMap::new();
    let mut cursor = String::new();
    let mut seen = BTreeSet::new();
    for _ in 0..MAX_MEMBER_PAGES {
        if missing.is_empty() {
            break;
        }
        let result = client::json_response(
            client::get(
                state,
                &format!("/im/v1/chats/{}/members", source.chat_id),
                token,
            )
            .query(&[
                ("member_id_type", "open_id"),
                ("page_size", "100"),
                ("page_token", cursor.as_str()),
            ]),
        )
        .await;
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!(code = error.1, "群成员姓名暂不可读，保留消息并显示缺名状态");
                break;
            }
        };
        let Some(members) = value["data"]["items"]
            .as_array()
            .filter(|items| items.len() <= 100)
        else {
            break;
        };
        for member in members {
            let (Some(id), Some(name)) = (member["member_id"].as_str(), member["name"].as_str())
            else {
                continue;
            };
            if !name.trim().is_empty() && missing.remove(id) {
                names.insert(id.to_owned(), name.chars().take(120).collect());
            }
        }
        if value["data"]["has_more"].as_bool() != Some(true) {
            break;
        }
        let next = value["data"]["page_token"].as_str().unwrap_or("");
        if next.is_empty() || next.len() > 16384 || !seen.insert(next.to_owned()) {
            break;
        }
        cursor = next.to_owned();
    }
    names
}
