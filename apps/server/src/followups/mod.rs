pub mod discovery;
pub mod policy;
pub mod routes;
pub mod scheduler;
mod service;
pub mod tools;
pub use service::{cancel_dependencies, create, preferences, save_preferences, update};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

// SELECT 明确列名，内部字段扩展不会改变 API 响应。
pub const COLUMNS: &str = "id,owner,conversation_id,kind,topic,due_at,expires_at,timezone,status,version,source_run_id,source_seq,observed_seq,memory_ids,memory_versions,lease_token,attempts,error,sent_at,updated_at";

/// 持久化的单次提醒/回访，版本和租约只用于防止旧执行器写回。
#[derive(Clone, Serialize, FromRow)]
pub struct Followup {
    /// 全生命周期稳定标识。
    pub id: Uuid,
    /// 真实会话身份，不从工具参数获取。
    pub owner: String,
    /// 通知归属的会话，也确定投递渠道。
    pub conversation_id: Uuid,
    /// reminder 按约定执行，checkin 需重新判断时机。
    pub kind: String,
    /// 独立可理解的事项，最多 500 字。
    pub topic: String,
    /// 下次尝试时间，始终存 UTC。
    pub due_at: DateTime<Utc>,
    /// 超时不补发，避免重启后集中发过期通知。
    pub expires_at: DateTime<Utc>,
    /// 创建/改期时的 IANA 时区，用于展示与解释时间。
    pub timezone: String,
    /// 调度与业务状态。
    pub status: String,
    /// 修改后递增，旧队列和模型输出不能继续使用。
    pub version: i64,
    /// 自动来源任务，手工事项为空。
    pub source_run_id: Option<Uuid>,
    /// 来源全局序号，与遗忘边界比较。
    pub source_seq: i64,
    /// 最后判断时见过的用户序号，出站延迟后仍能识别新信息。
    pub observed_seq: Option<i64>,
    /// 已验证属于同一身份的记忆依赖。
    pub memory_ids: Vec<Uuid>,
    /// 所用原文哈希用于发送前检测直接文件编辑及过期。
    #[serde(skip_serializing)]
    pub memory_versions: serde_json::Value,
    /// 内部领取令牌，不暴露到浏览器。
    #[serde(skip_serializing)]
    pub lease_token: Option<Uuid>,
    /// 同一版本的处理次数。
    pub attempts: i32,
    /// 可展示的错误分类，不保存上游正文。
    pub error: Option<String>,
    /// 实际确认发送的时间。
    pub sent_at: Option<DateTime<Utc>>,
    /// 最近状态变化时间。
    pub updated_at: DateTime<Utc>,
}
/// 创建输入中不允许指定 owner、接收人或内部状态。
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    /// 用户动作幂等键，自动工具由服务端产生。
    pub idempotency_key: Uuid,
    /// 可选现有会话；网页未指定时使用专用提醒会话。
    pub conversation_id: Option<Uuid>,
    /// reminder 或 checkin。
    pub kind: String,
    /// 提醒事项正文。
    pub topic: String,
    /// 带偏移的时间或在当前偏好时区下的本地分钟时间。
    pub due_at: String,
    /// 未指定时提醒默认一天、回访默认三天宽限。
    pub expires_at: Option<String>,
    /// 相关原文 ID，仅允许当前身份有效记忆。
    #[serde(default)]
    pub memory_ids: Vec<Uuid>,
    /// 用户在资料页面确认的条目；模型工具不能从外部资料自行生成此授权。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub communication: Option<crate::communications::Reference>,
}
/// 修改以版本号防止旧页面或旧模型覆盖刚发生的变化。
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    /// 客户端所见版本。
    pub version: i64,
    /// scheduled 表示改期，completed/cancelled 表示结束。
    pub status: String,
    /// 改期时必填。
    pub due_at: Option<String>,
    /// 可选同步修正事项。
    pub topic: Option<String>,
}
/// 用户偏好只约束轻量回访，明确提醒不受静默和频率策略抑制。
#[derive(Clone, Serialize, Deserialize, FromRow)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    /// IANA 时区，夏令时由时区库处理。
    pub timezone: String,
    /// 明确开启后才发现和发送回访。
    pub enabled: bool,
    /// 从本地午夜起的分钟数；起止相等表示无静默时段。
    pub quiet_start: i32,
    /// 可以跨午夜。
    pub quiet_end: i32,
    /// 两条回访的最短间隔。
    pub min_interval_minutes: i32,
    /// 并发编辑的乐观锁。
    pub version: i64,
}
/// 工具调用来源，每次落库前都重新验证执行租约与当前用户原文。
pub struct Source<'a> {
    /// 正在处理的任务。
    pub job: &'a crate::worker::Job,
    /// 同一动作在不同模型重试中的稳定键。
    pub operation_key: String,
    /// 后台发现使用完成任务；普通工具仅允许有效运行中的租约。
    pub discovery_attempt: Option<i32>,
}

/// 原文写入前先停止相关任务；文件写失败最多多取消一次，不能留下遗忘后的发送。
pub async fn service_memory_changed(
    state: &crate::AppState,
    owner: &str,
    id: Uuid,
    source: Option<Uuid>,
) -> crate::error::ApiResult<()> {
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT id FROM conversations WHERE owner=$1 ORDER BY id FOR UPDATE")
        .bind(owner)
        .execute(&mut *tx)
        .await?;
    cancel_dependencies(&mut tx, owner, id, source).await?;
    tx.commit().await?;
    Ok(())
}
