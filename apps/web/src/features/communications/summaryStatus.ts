import type { Detail } from './types';
import { processingError } from './processingError.ts';

/** 详情和测试共用状态解释，失败候选不能显示为“未发现事项”。 */
export function summaryNotice(detail: Detail) {
  const doc = detail.document;
  const error = doc.summary_error ? processingError(doc.summary_error) : '';
  switch (doc.summary_status) {
    case 'pending':
      return '已排队等待整理，刷新可查看最新状态。';
    case 'running':
      return '正在整理并核对引用，刷新可查看最新状态。';
    case 'retry_wait':
      return `本次整理暂未完成：${error}。后台将自动重试（已尝试 ${doc.summary_attempts ?? 1}/3 次）。`;
    case 'failed':
      return `整理失败：${error}。已停止自动重试，可手动重新整理。`;
    case 'partial': {
      const rejected = detail.summary?.rejected_count ?? 0;
      const missing = detail.summary?.failed_chunk_count ?? 0;
      return `部分整理完成，仅展示已通过核验的条目。${rejected ? `${rejected} 个候选未通过核验。` : ''}${missing ? '部分消息尚未完成整理。' : ''}已停止自动重试。`;
    }
    default:
      return !detail.summary
        ? error
          ? `整理失败：${error}`
          : '正在整理消息，原始内容可在下方查看。'
        : '';
  }
}

/** 仅终止状态提供重排，重复点击和在途更新由服务端版本校验兜底。 */
export function canRetrySummary(detail: Detail) {
  return (
    detail.source_enabled !== false &&
    (detail.document.summary_status === 'partial' || detail.document.summary_status === 'failed')
  );
}
