/** 待办正文与业务状态独立于安排和投递状态。 */
export type Content = {
  title: string;
  objective: string;
  completion_criteria: string;
  next_action: string;
  waiting_on: string;
  due_at: string | null;
};
export type Todo = Content & {
  id: string;
  status: string;
  version: number;
  updated_at: string;
};
/** 每个安排显式保存时区和投递目标，编辑入口不改变目标。 */
export type ScheduleInput = {
  kind: string;
  next_run_at: string;
  timezone: string;
  recurrence: string;
  ends_at: string | null;
  missed_policy: string;
  grace_minutes: number;
  instruction: string;
  channel: string;
};
export type Schedule = ScheduleInput & { id: string; status: string; version: number };
/** 每期结果来自持久化处理记录，未发送不能展示为送达。 */
export type Run = {
  version: number;
  completed_at: string | null;
  completion_note: string;
  id: string;
  scheduled_at: string;
  status: string;
  result: string | null;
  error: string | null;
};
export type Detail = {
  item: Todo;
  schedules: Schedule[];
  runs: Run[];
  events: { seq: number; kind: string; actor: string; created_at: string; detail: unknown }[];
  links: { id: string; kind: string; resource_id: string; label: string }[];
};
export type Listing = { items: Todo[]; total: number; offset: number; has_more: boolean };
export type Binding = { owner: string; enabled: boolean; version: number };
export const STATUS: Record<string, string> = {
  active: '进行中',
  waiting_external: '等待外部',
  needs_user: '需要我处理',
  completed: '已完成',
  cancelled: '已取消',
};
export const SCHEDULE_STATUS: Record<string, string> = {
  enabled: '已启用',
  paused: '已暂停',
  ended: '已结束',
};
export const KINDS: Record<string, string> = {
  reminder: '定时提醒',
  checkin: '主动回访',
  execute: '定时整理 / 查询',
};
export const RULES: Record<string, string> = {
  once: '单次',
  daily: '每天',
  weekdays: '工作日（周一至周五）',
  weekly: '每周',
  monthly: '每月',
};
export const RUN_STATUS: Record<string, string> = {
  scheduled: '等待处理',
  queued: '等待投递',
  checking: '处理中',
  sent: '已送达',
  failed: '失败',
  expired: '已过期',
  cancelled: '已取消',
  completed: '本次回访已结束',
  skipped: '已跳过',
};
/** 只提取正文，避免把旧版本和只读字段提交到服务端。 */
export function contentOf(item?: Todo): Content {
  return {
    title: item?.title ?? '',
    objective: item?.objective ?? '',
    completion_criteria: item?.completion_criteria ?? '',
    next_action: item?.next_action ?? '',
    waiting_on: item?.waiting_on ?? '',
    due_at: item?.due_at ?? null,
  };
}
/** 当地时间输入显式跟随安排时区，避开浏览器时区转换。 */
export function localTime(value: string, timezone: string): string {
  if (!value) return '';
  const parts = new Intl.DateTimeFormat('en-CA', {
    timeZone: timezone,
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hourCycle: 'h23',
  }).formatToParts(new Date(value));
  const get = (kind: string) => parts.find((p) => p.type === kind)?.value;
  return `${get('year')}-${get('month')}-${get('day')}T${get('hour')}:${get('minute')}`;
}
