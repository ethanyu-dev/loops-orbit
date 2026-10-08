import type { Document } from './types';

/** 日期目录只携带文件数量，不把原文或摘要加载到浏览器。 */
export interface DayFolder {
  /** 北京时间自然日。 */
  day: string;
  /** 当天全部来源的日文件数。 */
  count: number;
}
/** 日期目录使用日期游标，避免新导入记录挤动较早目录的位置。 */
export interface DayPage {
  /** 当前批次的日期目录。 */
  items: DayFolder[];
  /** 为空表示全部日期已加载。 */
  next_before: string | null;
}
/** 文件列表在已有文档标识上补充来源名称及订阅状态。 */
export interface LibraryFile extends Document {
  /** 群聊或联系人名称。 */
  source_label: string;
  /** 暂停来源的文件仍可浏览。 */
  source_enabled: boolean;
  /** 区分已移除与暂时暂停同步。 */
  source_subscribed?: boolean;
  /** 内容检索命中的原文片段，不是新的生成内容。 */
  excerpt?: string;
}
/** 服务端返回匹配总数和下一页，不将最近快照误作完整目录。 */
export interface FilePage {
  /** 本页最多五十份文件。 */
  items: LibraryFile[];
  /** 当前筛选下全部匹配文件数。 */
  total: number;
  /** 为空表示当前已经是最后一页。 */
  next_offset: number | null;
}

/** 把有序日期整理成月份目录，日期使用服务端自然日，不进行本地时区转换。 */
export function groupMonths(days: DayFolder[]) {
  const months = new Map<string, DayFolder[]>();
  for (const day of days) {
    const month = day.day.slice(0, 7);
    const entries = months.get(month) ?? [];
    entries.push(day);
    months.set(month, entries);
  }
  return [...months].map(([month, days]) => ({ month, days }));
}

/** 搜索或跨日期浏览时按日展示文件，同一天内保留接口返回顺序。 */
export function groupFiles(files: LibraryFile[]) {
  const groups = new Map<string, LibraryFile[]>();
  for (const file of files) {
    const entries = groups.get(file.day) ?? [];
    entries.push(file);
    groups.set(file.day, entries);
  }
  return [...groups].map(([day, files]) => ({ day, files }));
}

/** 核对中优先于旧摘要；文件存在不意味着整理成功。 */
export function fileStatus(file: Document) {
  if (file.extraction_version === 0) return { label: '核对中', kind: 'pending' };
  if (file.summary_error) return { label: '整理失败', kind: 'error' };
  if (file.summary_hash) return { label: '已整理', kind: 'ready' };
  return { label: '待整理', kind: 'pending' };
}

/** 浏览器查询参数不可信，分页只接受有限的非负整数。 */
export function readOffset(value: string | null) {
  const offset = Number(value);
  return Number.isSafeInteger(offset) && offset >= 0 && offset <= 4294967295 ? offset : 0;
}
