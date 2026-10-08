use super::{client, configured, subscription::valid_chat, unavailable};
use crate::{AppState, error::ApiResult};
use serde_json::Value;
use uuid::Uuid;

// 单次最多读取 50 个会话；一轮有界分页，完成后十分钟复查新增私聊。
const PAGE_SIZE: usize = 50;
const MAX_PAGES: i32 = 200;
const DISCOVERY_SECONDS: f64 = 600.0;
const RETRY_SECONDS: f64 = 300.0;

/// 持久化游标和连接版本共同阻止重连或断开前的旧结果写回。
#[derive(sqlx::FromRow)]
struct Candidate {
    /// 只服务管理员授权连接。
    owner: String,
    /// 连接版本，不包含凭证。
    version: i64,
    /// 每次新建连接生成，阻止删除重建后的版本号复用。
    private_discovery_generation: Uuid,
    /// 飞书不透明分页令牌，绝不显示到前端。
    discovery_cursor: String,
    /// 当前轮累计处理页数，防止异常分页无限扫描。
    discovery_pages: i32,
    /// 自动订阅只采集从此边界起的消息。
    private_subscription_since: i64,
}

/// 独立后台单步；网络读取不持有采集锁，提交时再次检查连接、排除记录及已有来源。
pub async fn step(state: &AppState) -> ApiResult<()> {
    configured(state)?;
    let candidate: Option<Candidate> = sqlx::query_as("SELECT owner,version,private_discovery_generation,discovery_cursor,discovery_pages,private_subscription_since FROM communication_connections WHERE owner='admin' AND status='active' AND auto_subscribe_private AND next_discovery<=now()")
        .fetch_optional(&state.pool).await?;
    let Some(candidate) = candidate else {
        return Ok(());
    };
    let result = async {
        let token = client::access(state).await?;
        let data = client::json_response(client::get(state, "/im/v1/chats", &token).query(&[
            ("types", "p2p"),
            ("page_size", "50"),
            ("user_id_type", "open_id"),
            ("sort_type", "ByActiveTimeDesc"),
            ("page_token", candidate.discovery_cursor.as_str()),
        ]))
        .await?;
        apply(state, &candidate, &data).await
    }
    .await;
    if let Err(error) = result {
        // 新连接或其他实例已推进游标时，旧失败不能覆盖其下一次发现时间。
        sqlx::query("UPDATE communication_connections SET discovery_error=$4,discovery_cursor='',discovery_pages=0,next_discovery=now()+make_interval(secs=>$5) WHERE owner=$1 AND version=$2 AND discovery_cursor=$3 AND discovery_pages=$6 AND private_discovery_generation=$7 AND next_discovery<=now() AND status='active' AND auto_subscribe_private")
            .bind(&candidate.owner).bind(candidate.version).bind(&candidate.discovery_cursor).bind(error.1).bind(RETRY_SECONDS).bind(candidate.discovery_pages).bind(candidate.private_discovery_generation).execute(&state.pool).await?;
    }
    Ok(())
}

/// 一页原子提交；只信任上游明确的 p2p 类型，即使服务端忽略 types 参数也不订阅群聊。
async fn apply(state: &AppState, candidate: &Candidate, data: &Value) -> ApiResult<()> {
    let items = data["data"]["items"]
        .as_array()
        .filter(|items| items.len() <= PAGE_SIZE)
        .ok_or_else(|| unavailable("无效会话页"))?;
    let more = data["data"]["has_more"]
        .as_bool()
        .ok_or_else(|| unavailable("缺少分页状态"))?;
    let next = if more {
        data["data"]["page_token"]
            .as_str()
            .filter(|cursor| {
                !cursor.is_empty() && cursor.len() <= 16384 && *cursor != candidate.discovery_cursor
            })
            .ok_or_else(|| unavailable("无效分页游标"))?
    } else {
        ""
    };
    let _guard = state.communications.lock().await;
    let mut tx = state.pool.begin().await?;
    let current: Option<i64> = sqlx::query_scalar("SELECT version FROM communication_connections WHERE owner=$1 AND version=$2 AND discovery_cursor=$3 AND discovery_pages=$4 AND private_discovery_generation=$5 AND status='active' AND auto_subscribe_private AND next_discovery<=now() FOR UPDATE")
        .bind(&candidate.owner).bind(candidate.version).bind(&candidate.discovery_cursor).bind(candidate.discovery_pages).bind(candidate.private_discovery_generation).fetch_optional(&mut *tx).await?;
    if current.is_none() {
        return Ok(());
    }
    for item in items {
        if item["chat_mode"] != "p2p" {
            continue;
        }
        let Some(chat_id) = item["chat_id"].as_str().filter(|id| valid_chat(id)) else {
            continue;
        };
        let label = item["name"]
            .as_str()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(chat_id)
            .chars()
            .take(120)
            .collect::<String>();
        // 已暂停、已保留资料的来源均不更新；被删除的私聊靠排除记录阻止重新添加。
        sqlx::query(include_str!("../sql/communication_private_subscribe.sql"))
            .bind(Uuid::new_v4())
            .bind(&candidate.owner)
            .bind(chat_id)
            .bind(label)
            .bind(candidate.private_subscription_since)
            .execute(&mut *tx)
            .await?;
    }
    let capped = more && candidate.discovery_pages + 1 >= MAX_PAGES;
    sqlx::query("UPDATE communication_connections SET discovery_cursor=$2,discovery_pages=$3,discovery_error=$4,next_discovery=now()+make_interval(secs=>$5) WHERE owner=$1")
        .bind(&candidate.owner).bind(if capped { "" } else { next }).bind(if more && !capped { candidate.discovery_pages + 1 } else { 0 })
        .bind(capped.then_some("communication_discovery_limit")).bind(if more && !capped { 2.0 } else { DISCOVERY_SECONDS }).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
