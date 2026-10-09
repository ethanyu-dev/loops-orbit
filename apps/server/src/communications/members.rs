use super::{Source, client};
use crate::AppState;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

// 每个消息页最多查询 20 页成员，避免异常游标或超大群无限阻塞采集；缺失名称不猜测。
const MAX_MEMBER_PAGES: usize = 20;
// 用户信息接口每批最多查询 50 人；只发送当前消息页仍缺名的 open_id。
const MAX_USER_BATCH: usize = 50;

/// 只补全当前页缺名的真人；群聊先查成员，单聊及群内未命中者通过用户信息补全。
pub(super) async fn names(
    state: &AppState,
    source: &Source,
    token: &str,
    items: &[Value],
    owner: &str,
    mode: &str,
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
    // 消息携带的提及映射也是同一 open_id 的可信展示名，可补足已离群成员的名称。
    for item in items {
        for mention in item["mentions"].as_array().into_iter().flatten() {
            let id = if mention["id_type"] == "open_id" {
                mention["id"].as_str()
            } else {
                mention["id"]["open_id"].as_str()
            };
            if let (Some(id), Some(name)) = (id, mention["name"].as_str())
                && !name.trim().is_empty()
                && missing.remove(id)
            {
                names.insert(id.to_owned(), name.trim().chars().take(120).collect());
            }
        }
    }
    let mut cursor = String::new();
    let mut seen = BTreeSet::new();
    for _ in 0..MAX_MEMBER_PAGES {
        if missing.is_empty() || mode == "p2p" {
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
                names.insert(id.to_owned(), name.trim().chars().take(120).collect());
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
    // 不使用可改名的会话标签或截图中的反应者/被回复者名称推断发送人。
    let ids: Vec<_> = missing.iter().collect();
    for batch in ids.chunks(MAX_USER_BATCH) {
        let mut query = vec![("user_id_type", "open_id")];
        query.extend(batch.iter().map(|id| ("user_ids", id.as_str())));
        let result = client::json_response(
            client::get(state, "/contact/v3/users/batch", token).query(&query),
        )
        .await;
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!(code = error.1, "用户姓名暂不可读，保留消息并显示缺名状态");
                break;
            }
        };
        for user in value["data"]["items"].as_array().into_iter().flatten() {
            if let (Some(id), Some(name)) = (user["open_id"].as_str(), user["name"].as_str())
                && batch.iter().any(|expected| expected.as_str() == id)
                && !name.trim().is_empty()
            {
                names.insert(id.to_owned(), name.trim().chars().take(120).collect());
            }
        }
    }
    names
}
