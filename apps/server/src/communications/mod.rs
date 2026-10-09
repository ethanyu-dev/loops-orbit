mod calendar;
mod client;
mod crypto;
pub(crate) mod dependencies;
mod extraction;
mod history;
mod images;
mod library;
mod members;
mod mentions;
mod oauth;
pub mod private_subscription;
pub mod progress;
mod removal;
mod retained;
pub mod routes;
pub(crate) mod search;
pub(crate) mod store;
pub mod subscription;
pub(crate) mod summary;
pub mod sync;
pub(crate) mod tools;
/// 与聊天共用的身份隔离检索入口。
pub use search::search as retrieve;

use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// 授权只申请只读会话及离线刷新，不申请以用户身份发送消息。
const SCOPES: &str = "offline_access im:chat:read im:message:readonly im:message.group_msg:get_as_user im:message.p2p_msg:get_as_user";
const CALLBACK: &str = "/api/communications/oauth/callback";
// 每次只处理一页，会话总量不再受手工选择上限限制。
pub(super) const LOCAL_TIMEZONE: chrono_tz::Tz = chrono_tz::Asia::Shanghai;

/// 采集显式启用；密钥与登录密钥独立，轮换后需要重新连接账号。
#[derive(Clone)]
pub struct Config {
    /// AES-256-GCM 密钥；不实现 Debug，防止日志泄露。
    pub token_key: [u8; 32],
}
impl Config {
    /// 禁用时不读取密钥，启用后必须给出 64 位十六进制密钥。
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        if !std::env::var("FEISHU_SYNC_ENABLED")
            .unwrap_or("false".into())
            .parse::<bool>()?
        {
            return Ok(None);
        }
        let bytes = hex::decode(std::env::var("FEISHU_TOKEN_KEY").unwrap_or_default())
            .map_err(|_| anyhow::anyhow!("FEISHU_TOKEN_KEY 必须为 64 位十六进制"))?;
        let token_key = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("FEISHU_TOKEN_KEY 必须为 64 位十六进制"))?;
        Ok(Some(Self { token_key }))
    }
}
/// 数据库白名单会话，不允许上游数据控制本地文件路径。
#[derive(Clone, Serialize, sqlx::FromRow)]
pub struct Source {
    /// 本地来源标识。
    pub id: Uuid,
    /// 飞书会话标识。
    pub chat_id: String,
    /// 用户可修改的展示名称。
    pub label: String,
    /// 日文件使用的时区，旧 UTC 文件由后台迁移。
    pub day_timezone: String,
    /// 已移除的订阅保留文件元数据，重新添加后才恢复采集。
    pub subscribed: bool,
    /// 暂停后不采集、不召回、不支持旧跟进。
    pub enabled: bool,
    /// 变更围栏，用于丢弃在途采集结果。
    pub version: i64,
    /// 用户选择的最早采集时间，Unix 秒。
    pub start_at: i64,
    /// 已完整分页的时间上界。
    pub watermark: i64,
    /// 当前固定窗口起点。
    pub window_start: Option<i64>,
    /// 当前固定窗口终点。
    pub window_end: Option<i64>,
    /// 上游不透明分页令牌，不能自行解码。
    #[serde(skip_serializing)]
    pub page_token: String,
    /// 下一次复查全部已选时间范围，以发现旧消息编辑或撤回。
    pub audit_at: chrono::DateTime<chrono::Utc>,
    /// 同步退避截止时间。
    pub next_sync: chrono::DateTime<chrono::Utc>,
    /// 完整窗口最近同步完成时间。
    pub last_synced_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 脱敏故障分类。
    pub error: Option<String>,
}
pub(super) const SOURCE_COLUMNS: &str = "id,chat_id,label,day_timezone,subscribed,enabled,version,start_at,watermark,window_start,window_end,page_token,audit_at,next_sync,last_synced_at,error";

/// 当前文件指针；正文读取时仍要校验哈希。
#[derive(Clone, Serialize, sqlx::FromRow)]
pub struct Document {
    /// 文档 ID。
    pub id: Uuid,
    /// 所属白名单会话。
    pub source_id: Uuid,
    /// 按来源时区划分的自然日。
    pub day: String,
    /// 原文内容指纹。
    pub raw_hash: String,
    /// 原文变更递增。
    pub version: i64,
    /// 个人关联规则的版本；旧资料必须先重处理。
    pub extraction_version: i64,
    /// 已验证摘要文件指纹；为空时不召回旧摘要。
    pub summary_hash: Option<String>,
    /// 摘要失败分类。
    pub summary_error: Option<String>,
}
pub(super) const DOCUMENT_COLUMNS: &str =
    "id,source_id,day,raw_hash,version,summary_hash,summary_error,extraction_version";

/// 提醒引用的是用户确认的摘要条目，不把整段外部对话当作用户指令。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    /// 本地原文文档。
    pub document_id: Uuid,
    /// 确认时所见版本。
    pub version: i64,
    /// 摘要中已校验证据的条目位置。
    pub item: usize,
}

/// 对外只输出稳定错误码，禁止附带上游响应或凭证。
pub(super) fn unavailable(_: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::SERVICE_UNAVAILABLE, "communication_unavailable")
}
/// 入口检查启用配置；内部调用同样拒绝缺失依赖。
pub(super) fn configured(state: &AppState) -> ApiResult<&Config> {
    state
        .config
        .communications
        .as_ref()
        .filter(|_| state.config.feishu.is_some() && state.config.memory.is_some())
        .ok_or(ApiError(StatusCode::CONFLICT, "communication_disabled"))
}
