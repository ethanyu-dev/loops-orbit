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
