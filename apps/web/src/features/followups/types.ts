/** 跟进列表与偏好均由当前登录身份限定。 */
export type Followup = {
  /** 修改操作使用的稳定标识和乐观锁。 */
  id: string;
  version: number;
  /** 消息最终进入的会话。 */
  conversation_id: string;
  /** 明确提醒或经允许的轻量回访。 */
  kind: 'reminder' | 'checkin';
  /** 可直接理解的事项。 */
  topic: string;
  /** 调度时间及其展示时区。 */
  due_at: string;
  timezone: string;
  /** 到期截止时间，超过后不补发。 */
  expires_at: string;
  /** 服务端调度状态，不由页面猜测。 */
  status: string;
  /** 错误分类不包含模型或供应商原文。 */
  error: string | null;
};
/** 仅回访遵守静默和冷却，明确提醒按约定时间执行。 */
export type Preferences = {
  /** 解释无偏移时间时使用的 IANA 时区。 */
  timezone: string;
  /** 显式开启后才能自动发现回访。 */
  enabled: boolean;
  /** 本地午夜后的分钟数，起止相同表示无静默。 */
  quiet_start: number;
  quiet_end: number;
  /** 两条回访的最短间隔。 */
  min_interval_minutes: number;
  /** 防止旧页面覆盖其他入口的修改。 */
  version: number;
};
/** 只有实际投递的消息会出现在通知中。 */
export type Notice = {
  /** 单条标记已读，避免吞掉同时到达的消息。 */
  id: number;
  /** 接续交流的原会话。 */
  conversation_id: string;
  /** 已投递的正文与时间。 */
  content: string;
  created_at: string;
  /** 已读和事项完成相互独立。 */
  read_at: string | null;
};
/** 页面与侧栏共用一个通知快照。 */
export type Notifications = { unread: number; items: Notice[] };
/** 事项列表和持久化偏好一次读取。 */
export type Snapshot = { items: Followup[]; preferences: Preferences };
