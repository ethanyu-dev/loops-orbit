use super::{
    super::{client, subscription::valid_chat},
    AGENT_PREFIX,
};
use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use serde_json::json;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

// 提示正文与回复共用同一个前缀，防止标识改动后遗漏防循环。
const NOTICE: &str = "正在绑定自聊测试。请在问题接管页确认保存成功后，再在此发送新问题；回答仅使用已发布的通用知识。";

/// 接收者固定为 OAuth 已验证的本人，不能由会话名称、历史发言或前端参数推断。
/// 只有用户在设置页显式开启时发送提示；失败时不开启，也不自动重试发送。
pub(super) async fn bind(state: &AppState, open_id: &str, token: &str) -> ApiResult<String> {
    let base = &state.config.feishu.as_ref().expect("已验证配置").api_base;
    let sent = client::json_response(state.http.post(format!("{base}/im/v1/messages"))
        .bearer_auth(token).query(&[("receive_id_type", "open_id")])
        .json(&json!({"receive_id":open_id,"msg_type":"text","content":json!({"text":format!("{AGENT_PREFIX}{NOTICE}")}).to_string(),"uuid":Uuid::new_v4().to_string()})))
        .await?;
    let data = &sent["data"];
    // 检查实际发送身份，避免应用身份的机器人会话被误绑定为本人自聊。
    let chat_id = data["chat_id"].as_str().filter(|id| valid_chat(id));
    if data["sender"]["id"] != open_id
        || data["sender"]["id_type"] != "open_id"
        || data["sender"]["sender_type"] != "user"
    {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "takeover_self_chat_unverified",
        ));
    }
    chat_id.map(str::to_owned).ok_or(ApiError(
        StatusCode::BAD_GATEWAY,
        "takeover_self_chat_unverified",
    ))
}

/// 开启测试即显式订阅本人自聊，只采集此刻起的新消息；现有资料起点不回退。
/// 正在遗忘的来源必须先完成清理，不能抢先恢复；关闭测试不删除已采集资料。
pub(super) async fn subscribe(tx: &mut Transaction<'_, Postgres>, chat_id: &str) -> ApiResult<()> {
    let changed = sqlx::query("INSERT INTO communication_sources(id,owner,chat_id,label,start_at,watermark,day_timezone,chat_mode) VALUES($1,'admin',$2,'本人自聊测试',floor(extract(epoch FROM clock_timestamp()))::bigint,floor(extract(epoch FROM clock_timestamp()))::bigint,'Asia/Shanghai','p2p') ON CONFLICT(owner,chat_id) DO UPDATE SET subscribed=true,enabled=true,chat_mode='p2p',version=communication_sources.version+1,next_sync=now() WHERE NOT communication_sources.removal_pending")
        .bind(Uuid::new_v4()).bind(chat_id).execute(&mut **tx).await?;
    if changed.rows_affected() != 1 {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "takeover_self_chat_removing",
        ));
    }
    sqlx::query("DELETE FROM communication_exclusions WHERE owner='admin' AND chat_id=$1")
        .bind(chat_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
