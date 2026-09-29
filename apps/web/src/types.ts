/** 会话接口返回的身份及展示配置；认证凭据只保存在 HttpOnly Cookie 中。 */
export type Session = {
  /** 服务端每次请求重新校验的访问身份。 */
  identity: {
    /** 是否显示管理员功能，实际权限仍由服务端校验。 */
    admin: boolean;
    /** 会话归属标识，不作为前端授权依据。 */
    owner: string;
    /** 当前身份的到期时间。 */
    expires_at: string;
  };
  /** 界面显示的模型名称。 */
  model: string;
  /** 服务端是否配置了飞书入口。 */
  feishu_enabled: boolean;
};

/** 侧栏使用的会话摘要，详情按需单独加载。 */
export type Conversation = {
  /** 会话的稳定标识。 */
  id: string;
  /** 首条输入生成的短标题。 */
  title: string;
  /** 区分网页与飞书入口。 */
  channel: 'web' | 'feishu';
  /** 服务端最近提交时间，用于会话排序。 */
  updated_at: string;
};

/** 可见消息不包含模型内部的工具调用记录。 */
export type Message = {
  /** 消息序号，作为渲染与复制状态的标识。 */
  id: number;
  /** 决定用户文本与助手 Markdown 的展示方式。 */
  role: 'user' | 'assistant';
  /** 消息正文。 */
  content: string;
  /** 对应任务，用于关联执行失败提示。 */
  run_id: string | null;
  /** 主动消息没有伪造的用户任务。 */
  kind: 'conversation' | 'followup';
  /** 主动消息的原事项。 */
  followup_id: string | null;
};

/** 持久化任务摘要，前端轮询跟踪其状态。 */
export type Run = {
  /** 任务的稳定标识。 */
  id: string;
  /** 排队、执行、完成或失败状态。 */
  status: 'queued' | 'running' | 'completed' | 'failed' | 'superseded' | 'cancelled';
  /** 未完成回复的持久化快照。 */
  partial_content: string;
  /** 上下文整理与生成阶段，展示真实进度。 */
  phase: string;
  /** 服务端提供的稳定错误分类。 */
  error: string | null;
};

/** 会话详情同时提供按顺序排列的消息与任务摘要。 */
export type Detail = {
  /** 服务端按对话轮次排列的可见消息。 */
  messages: Message[];
  /** 近期任务状态，用于排队提示和发送约束。 */
  runs: Run[];
};

/** 授权列表只返回元数据，不能恢复原始访问 token。 */
export type AccessLink = {
  /** 撤销操作使用的授权标识。 */
  id: string;
  /** 管理员填写的用途名称。 */
  label: string;
  /** 授权截止时间。 */
  expires_at: string;
  /** 已撤销时记录撤销时间。 */
  revoked_at: string | null;
  /** 授权创建时间。 */
  created_at: string;
};

/** 数据库中的全局统计快照，不代表模型或飞书实时连通性。 */
export type Status = {
  /** 各执行状态下的任务总数。 */
  runs: Record<Run['status'], number>;
  /** 飞书回复投递队列的状态计数。 */
  delivery: Record<Run['status'], number>;
  /** 服务端实际配置的模型标识。 */
  model: string;
  /** 服务端是否配置飞书入口。 */
  feishu_enabled: boolean;
  /** 单实例的并行执行数量。 */
  workers_per_instance: number;
  /** 当前运行的服务端版本。 */
  version: string;
};
