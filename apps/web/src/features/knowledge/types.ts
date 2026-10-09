/** 管理员专用的知识投影；原文证据不发送给他人问答模型。 */
export type KnowledgeEntry = {
  /** 服务端稳定标识。 */
  id: string;
  /** 编辑和发布须携带的版本。 */
  version: number;
  /** 可对外显示的主题。 */
  title: string;
  /** 确认后可对外复用的正文。 */
  content: string;
  /** 随正文审批的可检索主题标签。 */
  tags: string[];
  /** 审核和对外使用状态。 */
  status: 'candidate' | 'published' | 'rejected' | 'revoked';
  /** 自动扫描的北京时间日期；手工知识为空，与沟通文件无关联。 */
  extraction_day: string | null;
  /** 仅供本人审阅的连续原话。 */
  evidence: { quote: string; snapshot_id?: string; source_label?: string; source_day?: string }[];
  /** 最近修改时间。 */
  updated_at: string;
};

/** 有界分页和后台状态，未发现候选不代表已处理全部消息。 */
export type KnowledgeSnapshot = {
  /** 当前页候选或已发布知识。 */
  items: KnowledgeEntry[];
  /** 匹配筛选条件的总数。 */
  total: number;
  /** 当前页偏移。 */
  offset: number;
  /** 是否还有下一页。 */
  has_more: boolean;
  /** 是否具备日常沟通提取条件。 */
  extraction_enabled: boolean;
  /** 超长消息跳过数独立显示，不能当作已处理。 */
  /** 后台向量索引进度；不影响已发布内容的关键词检索。 */
  rag: { chunks: number; embedded: number; retrying: number };
  jobs: { pending: number; failed: number; skipped: number };
};

// 与服务端审核状态保持一致，沟通原文变化不影响知识。
export const KNOWLEDGE_STATUS = {
  candidate: '待确认',
  published: '已发布',
  rejected: '已拒绝',
  revoked: '已撤回',
};

/** 手动提取的执行进度，不携带原文快照或来源链接。 */
export type ExtractionTask = {
  /** 与沟通文件无关的任务标识。 */
  id: string;
  /** 自动扫描与手动提取共用状态结构。 */
  kind: 'daily' | 'manual';
  /** 被整理内容所属日期。 */
  scan_day: string;
  /** 完成可能没有新增候选，不能等同于已发布知识。 */
  status: 'queued' | 'running' | 'completed' | 'failed';
  /** 已处理的合格文本消息数。 */
  next_offset: number;
  /** 超预算跳过消息数。 */
  skipped_count: number;
  /** 实际新增候选数量。 */
  created_count: number;
  /** 固定故障类别。 */
  error: string | null;
};
