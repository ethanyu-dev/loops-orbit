pub(crate) mod execution;
pub mod identity;
pub mod recurrence;
mod resources;
pub mod routes;
pub mod scheduler;
pub mod service;
pub mod tools;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// 个人待办和输入预算，防止后台安排无限增长。
pub const MAX_TODOS: i64 = 1000;
pub const MAX_SCHEDULES: i64 = 20;
pub const MAX_TEXT: usize = 4000;
pub const TODO_COLUMNS: &str = "id,owner,title,objective,completion_criteria,next_action,waiting_on,status,due_at,version,created_at,updated_at,closed_at";

/// 独立于会话的事项快照，发送通知不会自动完成事项。
#[derive(Serialize, sqlx::FromRow)]
pub struct Todo {
    /// 稳定 ID 与服务端确定的数据主体。
    pub id: Uuid,
    /// 经验证的本人数据主体。
    pub owner: String,
    /// 用户的目标、完成条件和下一步。
    pub title: String,
    /// 希望实现的目标。
    pub objective: String,
    /// 本人确认完成时参考的条件。
    pub completion_criteria: String,
    /// 当前可推进的行动。
    pub next_action: String,
    /// 依赖的人或外部条件。
    pub waiting_on: String,
    /// 业务状态及可选截止时间。
    pub status: String,
    /// 与通知时间独立的业务截止时间。
    pub due_at: Option<DateTime<Utc>>,
    /// 跨渠道并发修改的乐观锁。
    pub version: i64,
    /// 创建、变更和结束时间。
    pub created_at: DateTime<Utc>,
    /// 最近一次已提交的变更时间。
    pub updated_at: DateTime<Utc>,
    /// 整个事项结束时间，重新打开时清除。
    pub closed_at: Option<DateTime<Utc>>,
}
/// 表单和工具共用的完整可编辑正文，更新时显式保留原值。
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Content {
    /// 简短名称，其余说明字段可以为空。
    pub title: String,
    /// 用户希望实现的目标，可以暂时不填写。
    #[serde(default)]
    pub objective: String,
    /// 明确事项结束所需的条件。
    #[serde(default)]
    pub completion_criteria: String,
    /// 下一步可以采取的行动。
    #[serde(default)]
    pub next_action: String,
    /// 等待的对象或外部条件。
    #[serde(default)]
    pub waiting_on: String,
    /// 空值清除截止时间，不改变任何执行安排。
    pub due_at: Option<DateTime<Utc>>,
}
/// 创建动作由客户端生成稳定幂等键；身份和来源由宿主注入。
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    /// 同一创建请求重试时保持不变。
    pub idempotency_key: Uuid,
    /// 初始事项正文。
    pub content: Content,
    /// 可选初始安排，与事项在同一事务创建。
    pub schedule: Option<ScheduleInput>,
}
/// 更新采用完整正文和预期版本，冲突时客户端必须重读。
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    /// 拒绝过期写入的预期版本。
    pub version: i64,
    /// 包含未改字段的完整正文。
    pub content: Content,
    /// 本次明确设置的业务状态。
    pub status: String,
}
/// 安排以显式时区解释本地时间，重复规则保持当地钟点。
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleInput {
    /// reminder 到点提醒，checkin 判断回访，execute 使用只读工具整理结果。
    pub kind: String,
    /// 支持显式偏移或指定时区的本地时间。
    pub next_run_at: String,
    /// 日历重复规则使用的 IANA 时区。
    pub timezone: String,
    /// 单次或受支持的日历重复规则。
    pub recurrence: String,
    /// 停止产生新期次的时间边界。
    pub ends_at: Option<DateTime<Utc>>,
    /// 错过周期时跳过或只补最近一期。
    pub missed_policy: String,
    /// 允许补发的最大延迟分钟数。
    pub grace_minutes: i32,
    /// 提醒内容或后台只读整理要求。
    #[serde(default)]
    pub instruction: String,
    /// web 或 feishu；修改安排时必须显式保留当前目标。
    pub channel: String,
}
/// 每个安排独立暂停、改期与结束，待办可拥有多个安排。
#[derive(Serialize, sqlx::FromRow)]
pub struct Schedule {
    /// 稳定安排标识。
    pub id: Uuid,
    /// 所属事项，暂停和改期不会改变归属。
    pub todo_id: Uuid,
    /// 提醒、回访或只读整理。
    pub kind: String,
    /// 启用、暂停或结束。
    pub status: String,
    /// 下一个尚未产生期次的时间。
    pub next_run_at: DateTime<Utc>,
    /// 原始日历锚点，防止月末截断造成日期漂移。
    pub anchor_at: DateTime<Utc>,
    /// 重复规则所属时区。
    pub timezone: String,
    /// 日历重复方式。
    pub recurrence: String,
    /// 可选的终止时间。
    pub ends_at: Option<DateTime<Utc>>,
    /// 停机恢复后的补发策略。
    pub missed_policy: String,
    /// 补发宽限分钟数。
    pub grace_minutes: i32,
    /// 模型或提醒所需的本项说明。
    pub instruction: String,
    /// 本安排通知的落地会话。
    pub conversation_id: Uuid,
    /// 原渠道身份，由服务端验证和确定。
    pub delivery_owner: String,
    /// 绑定版本变化使旧安排失效。
    pub binding_version: Option<i64>,
    /// 修改安排时递增，隔离已生成的旧期次。
    pub version: i64,
}
/// 工具调用携带真实来源和租约，API 操作不伪造模型任务。
pub struct Source<'a> {
    /// 宿主提供的真实任务和租约。
    pub job: &'a crate::worker::Job,
    /// 当前工具写入的稳定操作键。
    pub operation_key: String,
    /// 工具宿主创建时的本人绑定版本。
    pub identity_version: Option<i64>,
}
