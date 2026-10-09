import { useState } from 'react';
import { api } from '../../api';
import type { Snapshot } from './types';

/** 删除任务由后台推进，页面关闭不影响处理；重试仅针对失败项。 */
export function RemovalProgress({
  jobs,
  reload,
  report,
}: {
  /** 每次状态轮询返回的持久化进度。 */
  jobs: NonNullable<Snapshot['removals']>;
  /** 重试提交后读取服务端结果。 */
  reload: () => Promise<void>;
  /** 复用页面错误提示。 */
  report: (error: unknown) => void;
}) {
  const [retrying, setRetrying] = useState<string | null>(null);
  /** 不重建原批次，不重复删除成功项。 */
  async function retry(id: string) {
    if (retrying) return;
    setRetrying(id);
    try {
      await api(`/communications/removals/${id}/retry`, { method: 'POST' });
      await reload();
    } catch (error) {
      report(error);
    } finally {
      setRetrying(null);
    }
  }
  return (
    <div className="subscription-removal-jobs" aria-label="后台删除任务">
      {jobs.map((job) => (
        <div className="subscription-notice" key={job.id}>
          <span>
            {job.pending ? '正在后台删除' : job.failed ? '删除任务部分失败' : '删除已完成'}：
            {job.complete}/{job.total} 个会话
          </span>
          {job.pending > 0 && <span> · 待处理 {job.pending} 个，可关闭页面</span>}
          {job.failed > 0 && (
            <>
              <span> · 失败 {job.failed} 个</span>
              <button disabled={!!retrying} onClick={() => void retry(job.id)}>
                {retrying === job.id ? '正在提交…' : '重试失败项'}
              </button>
            </>
          )}
        </div>
      ))}
    </div>
  );
}
