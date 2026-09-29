/** 来源和文件版本由服务端维护，浏览器不保存飞书凭证。 */
export interface Source {
  /** 本地选择记录。 */
  id: string;
  /** 上游会话标识，仅用于展示。 */
  chat_id: string;
  /** 用户选择时的名称。 */
  label: string;
  /** 同时控制采集与检索。 */
  enabled: boolean;
  /** 防止旧页面覆盖新状态。 */
  version: number;
  /** 完整分页的最近完成时间。 */
  last_synced_at: string | null;
  /** 非空表示仍在导入当前窗口。 */
  window_end: number | null;
  /** 稳定错误分类。 */
  error: string | null;
}
/** 本地日文件的可审阅版本。 */
export interface Document {
  /** 文档标识。 */
  id: string;
  /** 所属会话选择。 */
  source_id: string;
  /** 北京时间自然日。 */
  day: string;
  /** 引用绑定版本。 */
  version: number;
  /** 有值表示已完成整理。 */
  summary_hash: string | null;
  /** 摘要故障分类。 */
  summary_error: string | null;
}
/** 每个归纳都可核对原话，承诺归属由服务端检查。 */
export interface Item {
  /** 分类名称。 */
  kind: string;
  /** 机器归纳，不是已确认个人事实。 */
  text: string;
  /** 精确原话。 */
  quote: string;
  /** 飞书消息标识。 */
  message_id: string;
  /** 原始发送者。 */
  sender_id: string;
  /** 服务端解析的可读名称，缺失时使用会话成员。 */
  sender_name: string;
  /** 已授权账号是否为发送者。 */
  is_me: boolean;
  /** 毫秒时间。 */
  create_time: number;
}
/** 分页详情明确未解析消息的范围。 */
export interface Detail {
  /** 文件版本。 */
  document: Document;
  /** 尚未完成整理时为空。 */
  summary: { items: Item[]; unsupported_count: number; message_count: number } | null;
  /** 原始记录总数。 */
  total: number;
  /** 当前页原文，附件仅显示类型。 */
  messages: {
    message_id: string;
    text: string;
    sender_id: string;
    /** 服务端解析的可读名称，缺失时使用会话成员。 */
    sender_name: string;
    is_me: boolean;
    create_time: number;
    deleted: boolean;
    message_type: string;
    images: {
      url: string;
      description: string | null;
      error: string | null;
      reference_only: boolean;
    }[];
  }[];
}
/** 只包含可显示状态，任何令牌均不返回前端。 */
export interface Snapshot {
  /** 服务端是否启用采集。 */
  enabled: boolean;
  /** 已授权账号，不等于所有管理员可见的机器人用户。 */
  connection: {
    name: string;
    open_id: string;
    status: string;
    auto_subscribe: boolean;
    subscription_since: number;
    discovery_error: string | null;
  } | null;
  /** 历史任务与增量同步分开展示。 */
  history_jobs: {
    id: string;
    source_id: string;
    start_at: number;
    end_at: number;
    status: string;
    error: string | null;
  }[];
  /** 全部日文件和图片的处理统计，不局限于当前列表页。 */
  progress: {
    source_id: string;
    total: number;
    ready: number;
    summarizing: number;
    indexing: number;
    errors: number;
    images: number;
    images_ready: number;
    images_failed: number;
  }[];
  /** 用户逐个选择的会话。 */
  sources: Source[];
  /** 最近 100 份日文件，完整历史通过检索访问。 */
  documents: Document[];
}
