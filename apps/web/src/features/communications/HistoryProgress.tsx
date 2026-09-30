import { useEffect, useState } from 'react';
import { api, errorText } from '../../api';
import type { Snapshot } from './types';

// 轻量元数据每五秒读取一次，上一请求结束后再排下一轮，避免慢网络叠加请求。
const POLL_INTERVAL = 5000;
const STATES: Record<string, string> = {
  pending: '等待拉取',
  running: '正在分页拉取',
  complete: '本轮消息已拉取',
  failed: '拉取失败，等待重试',
  cancelled: '已取消历史采集',
};
/** 全量任务统计与有限的活动列表；分页数是累计工作量，不等于唯一消息数。 */
interface Progress {
  /** 全部历史范围任务的统计，不限于当前活动列表。 */
  counts: {
    /** 任务总数，包括暂停来源。 */
    total: number;
    /** 启用来源中等待首次拉取的任务。 */
    pending: number;
    /** 正在分页的任务，包含等待下一页。 */
    running: number;
    /** 启用来源中本轮拉取完成的任务。 */
    complete: number;
    /** 启用来源中等待重试的任务。 */
    failed: number;
    /** 暂停来源的任务，不计入其他状态。 */
    paused: number;
    /** 已取消任务，包含原先暂停的来源。 */
    cancelled: number;
    /** 累计成功分页，包含重放与复查。 */
    pages_processed: number;
    /** 最近成功分页时间；旧任务可能尚无记录。 */
    last_progress_at: string | null;
  };
  /** 优先失败与进行中任务，最多 20 条。 */
  jobs: {
    /** 本地任务标识，仅用于渲染关联。 */
    id: string;
    /** 可读会话名称。 */
    label: string;
    /** 来源是否仍允许采集。 */
    enabled: boolean;
    /** 历史窗口起点，Unix 秒。 */
    start_at: number;
    /** 历史窗口排他终点，Unix 秒。 */
    end_at: number;
    /** 后台队列状态。 */
    status: string;
    /** 稳定错误类别，不含原始供应商响应。 */
    error: string | null;
    /** 此任务累计成功分页。 */
    pages_processed: number;
    /** 此任务最近成功分页时间。 */
    last_progress_at: string | null;
    /** 最早可重试时间，不保证实际执行时刻。 */
    next_attempt: string;
  }[];
  /** 有效连接是后台处理的必要条件。 */
  connected: boolean;
}
/** 以北京时间显示成功推进时间，未取得进度时不暗示任务已经停止。 */
function time(value: string) {
  return new Date(value).toLocaleString('zh-CN', { timeZone: 'Asia/Shanghai' });
}
/** 顶部常驻进度区，拉取任务和整个工作空间的资料处理使用不同计量。 */
export function HistoryProgress({
  revision,
  documents,
}: {
  revision: number;
  documents: Snapshot['progress'];
}) {
  const [data, setData] = useState<Progress | null>(null);
  const [error, setError] = useState('');
  const [updated, setUpdated] = useState('');
  const [retry, setRetry] = useState(0);
  const [cancelling, setCancelling] = useState(false);
  const [notice, setNotice] = useState('');
  /** 取消停止历史拉取和复查，不删除已有资料，也不暂停新消息。 */
  async function cancel(id?: string) {
    setCancelling(true);
    try {
      const result = await api<{ count: number }>('/communications/history/cancel', {
        method: 'POST',
        body: JSON.stringify(id ? { scope: 'job', id } : { scope: 'all' }),
      });
      setNotice(
        `已取消 ${result.count} 个历史任务。已导入资料保留并继续整理，新消息订阅不受影响。`,
      );
      setRetry((value) => value + 1);
    } catch (reason) {
      setNotice(errorText(reason));
    } finally {
      setCancelling(false);
    }
  }
  useEffect(() => {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    async function load() {
      try {
        const value = await api<Progress>('/communications/history/progress', {
          signal: controller.signal,
        });
        if (!controller.signal.aborted) {
          setData(value);
          setError('');
          setUpdated(new Date().toISOString());
        }
      } catch (reason) {
        if (!controller.signal.aborted) setError(errorText(reason));
      } finally {
        if (!controller.signal.aborted) timer = setTimeout(() => void load(), POLL_INTERVAL);
      }
    }
    void load();
    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, [revision, retry]);
  const totals = documents.reduce(
    (sum, row) => ({
      total: sum.total + row.total,
      checking: sum.checking + (row.checking || 0),
      ready: sum.ready + row.ready,
      summarizing: sum.summarizing + row.summarizing,
      indexing: sum.indexing + row.indexing,
      errors: sum.errors + row.errors,
      images: sum.images + row.images,
      images_ready: sum.images_ready + row.images_ready,
      images_failed: sum.images_failed + row.images_failed,
    }),
    {
      total: 0,
      checking: 0,
      ready: 0,
      summarizing: 0,
      indexing: 0,
      errors: 0,
      images: 0,
      images_ready: 0,
      images_failed: 0,
    },
  );
  const counts = data?.counts;
  const active = counts ? counts.total - counts.paused - counts.cancelled : 0;
  return (
    <section
      id="history-progress"
      tabIndex={-1}
      className="settings-card communication-card history-progress"
      aria-labelledby="history-progress-title"
    >
      <div className="communication-row">
        <h2 id="history-progress-title">历史整理进度</h2>
        <div className="source-actions">
          <button onClick={() => setRetry((value) => value + 1)}>刷新进度</button>
          <button
            disabled={cancelling || !counts || counts.total === counts.cancelled}
            onClick={() => void cancel()}
          >
            {cancelling ? '取消中…' : '取消全部历史任务'}
          </button>
        </div>
      </div>
      {notice && <p role="status">{notice}</p>}
      <p>
        取消会停止历史拉取和每日复查；已导入资料保留并继续整理，新消息订阅不受影响。再次提交相同范围可重新开始。
      </p>
      {error && (
        <p role="alert">
          进度暂未更新：{error} {data ? '以下保留上次结果。' : ''}
        </p>
      )}
      {!data ? (
        <p role="status">{error ? '等待重新读取进度。' : '正在读取历史任务…'}</p>
      ) : (
        counts && (
          <>
            {!data.connected && (
              <p role="alert">飞书连接当前不可用，任务会保留；请重新授权后继续。</p>
            )}
            {!counts.total ? (
              <p>尚未提交历史任务。在下方选择会话，或一键整理全部会话近半年。</p>
            ) : (
              <>
                <p>
                  <strong>
                    {active > 0
                      ? `本轮拉取完成 ${counts.complete} / ${active} 个启用任务`
                      : '当前没有启用的历史任务'}
                  </strong>{' '}
                  · 共 {counts.total} 个任务（包含暂停与取消）
                </p>
                {active > 0 && (
                  <progress
                    aria-label="历史任务本轮拉取完成比例"
                    value={counts.complete}
                    max={active}
                  />
                )}
                <div className="history-progress-counts">
                  <span>
                    等待拉取 <strong>{counts.pending}</strong>
                  </span>
                  <span>
                    正在拉取 <strong>{counts.running}</strong>
                  </span>
                  <span className={counts.failed ? 'error-text' : ''}>
                    失败重试 <strong>{counts.failed}</strong>
                  </span>
                  <span>
                    已暂停 <strong>{counts.paused}</strong>
                  </span>
                  <span>
                    已取消 <strong>{counts.cancelled}</strong>
                  </span>
                </div>
                <p>
                  累计成功处理 {counts.pages_processed} 页（含重放与复查）；
                  {counts.last_progress_at
                    ? `最近推进：${time(counts.last_progress_at)}`
                    : '等待新的分页完成记录'}
                  。分页计数从本次功能上线起记录。
                </p>
              </>
            )}
            <div className="history-processing-summary">
              <h3>资料处理 · 整个工作空间</h3>
              <p>
                已保存 {totals.total} 份日资料，文字资料可用 {totals.ready} 份；待整理{' '}
                {totals.summarizing} 份，待索引 {totals.indexing} 份，失败重试 {totals.errors} 份。
              </p>
              <p>
                图片解读 {totals.images_ready} / {totals.images} 张
                {totals.images_failed ? `，${totals.images_failed} 张失败重试中` : ''}。
              </p>
              <p>
                处理统计由后台分批核对，可能稍有延迟。
                {totals.checking > 0 &&
                  `另有 ${totals.checking} 份资料统计更新中；图片计数待核对后补齐。`}
                拉取完成后仍需整理与索引。这里包含新消息与历史消息，同一天同一会话合并为一份资料。
              </p>
            </div>
            {data.jobs.length > 0 && (
              <details className="history-jobs">
                <summary>查看任务详情 · 优先显示失败与进行中的任务</summary>
                <p>
                  最多显示 20
                  个任务；上方统计包含全部任务。已完成的范围每日复查，因此本轮状态会变化。
                </p>
                {data.jobs.map((job) => (
                  <article className="communication-evidence" key={job.id}>
                    <strong>{job.label}</strong>
                    <p>
                      {job.status === 'cancelled'
                        ? STATES.cancelled
                        : job.enabled
                          ? STATES[job.status]
                          : '已暂停'}{' '}
                      ·{' '}
                      {new Date(job.start_at * 1000).toLocaleDateString('zh-CN', {
                        timeZone: 'Asia/Shanghai',
                      })}
                      —
                      {new Date((job.end_at - 1) * 1000).toLocaleDateString('zh-CN', {
                        timeZone: 'Asia/Shanghai',
                      })}{' '}
                      · 累计 {job.pages_processed} 页
                    </p>
                    {job.status !== 'cancelled' && (
                      <button disabled={cancelling} onClick={() => void cancel(job.id)}>
                        取消此历史任务
                      </button>
                    )}
                    {job.error && job.enabled && (
                      <p className="error-text">
                        {job.error === 'communication_provider_rejected'
                          ? '飞书拒绝读取，请检查授权和消息权限。'
                          : '暂时拉取失败，后台会自动重试。'}{' '}
                        下次尝试：{time(job.next_attempt)}
                      </p>
                    )}
                  </article>
                ))}
              </details>
            )}
          </>
        )
      )}
      {updated && <small>每 5 秒自动刷新任务 · 最近读取 {time(updated)}（北京时间）</small>}
    </section>
  );
}
