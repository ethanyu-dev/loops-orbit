import { useEffect, useState } from 'react';
import { api, errorText } from '../../api';
import type { Snapshot } from './types';
import { Link } from 'react-router-dom';
import { RefreshCw } from 'lucide-react';
import { HistoryTasks } from './HistoryTasks';

// 轻量元数据每五秒读取一次，上一请求结束后再排下一轮，避免慢网络叠加请求。
// 单次读取超时后保留旧快照并重试，避免网络挂起冻结整个进度区。
const REQUEST_TIMEOUT = 20000;
const POLL_INTERVAL = 5000;
/** 全量任务统计与有限的活动列表；分页数是累计工作量，不等于唯一消息数。 */
export interface Progress {
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
  onRefresh,
}: {
  revision: number;
  documents: Snapshot['progress'];
  /** 手动刷新时同时更新工作空间资料统计。 */
  onRefresh: () => Promise<void>;
}) {
  const [data, setData] = useState<Progress | null>(null);
  const [error, setError] = useState('');
  const [updated, setUpdated] = useState('');
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    async function load() {
      try {
        const value = await api<Progress>('/communications/history/progress', {
          signal: AbortSignal.any([controller.signal, AbortSignal.timeout(REQUEST_TIMEOUT)]),
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
      className="sync-progress"
      aria-labelledby="history-progress-title"
    >
      <div className="sync-heading">
        <div>
          <h2 id="history-progress-title">处理进度</h2>
          <p>{updated ? `任务每 5 秒自动更新 · 最近读取 ${time(updated)}` : '正在读取任务状态…'}</p>
        </div>
        <button
          className="sync-button"
          onClick={() => {
            setRetry((value) => value + 1);
            void onRefresh();
          }}
        >
          <RefreshCw size={14} />
          刷新
        </button>
      </div>
      {error && (
        <p className="sync-alert" role="alert">
          进度暂未更新：{error} {data ? '以下保留上次结果。' : '正在重试。'}
        </p>
      )}
      {data && !data.connected && (
        <p className="sync-alert" role="alert">
          飞书连接不可用，任务已保留，请重新授权后继续。
        </p>
      )}
      <div className="sync-overview">
        <article>
          <div className="sync-stage-title">
            <span>01</span>
            <h3>拉取消息</h3>
            <small>历史任务</small>
          </div>
          <div className="sync-metric">
            <strong>{counts ? counts.complete : '—'}</strong>
            <span>/ {counts ? active : '—'} 个启用任务完成本轮拉取</span>
          </div>
          <progress
            aria-label="历史任务本轮拉取完成比例"
            value={counts?.complete ?? 0}
            max={Math.max(active, 1)}
          />
          <div className="sync-counts">
            <span>
              等待 <b>{counts?.pending ?? '—'}</b>
            </span>
            <span>
              进行中 <b>{counts?.running ?? '—'}</b>
            </span>
            <span className={counts?.failed ? 'error-text' : ''}>
              失败重试 <b>{counts?.failed ?? '—'}</b>
            </span>
          </div>
          <p className="sync-note">
            已暂停 {counts?.paused ?? '—'} · 已取消 {counts?.cancelled ?? '—'} · 累计{' '}
            {counts?.pages_processed ?? '—'} 页
          </p>
        </article>
        <article>
          <div className="sync-stage-title">
            <span>02</span>
            <h3>整理资料</h3>
            <small>整个工作空间</small>
          </div>
          <div className="sync-metric">
            <strong>{totals.ready}</strong>
            <span>/ {totals.total} 份日资料可用</span>
          </div>
          <progress
            aria-label="工作空间文字资料可用比例"
            value={totals.ready}
            max={Math.max(totals.total, 1)}
          />
          <div className="sync-counts">
            <span>
              待整理 <b>{totals.summarizing}</b>
            </span>
            <span>
              待索引 <b>{totals.indexing}</b>
            </span>
            <span className={totals.errors ? 'error-text' : ''}>
              失败重试 <b>{totals.errors}</b>
            </span>
          </div>
          <p className="sync-note">
            图片已解读 {totals.images_ready} / {totals.images} 张
            {totals.images_failed > 0 && ` · ${totals.images_failed} 张失败`}
            {totals.checking > 0 && ` · ${totals.checking} 份统计更新中`}
          </p>
        </article>
      </div>
      {(totals.errors > 0 || totals.images_failed > 0) && (
        <div className="sync-alert">
          {totals.errors > 0 ? `${totals.errors} 份资料处理失败` : '部分图片解读失败'}
          ，后台会自动重试。
          <Link
            to={`/communications/records?day=all&status=${totals.errors > 0 ? 'failed' : 'images_failed'}`}
          >
            查看失败资料 →
          </Link>
        </div>
      )}
      <details className="sync-explanation">
        <summary>进度如何计算</summary>
        <p>
          拉取完成后仍需整理与索引。资料统计包含新消息和历史消息，同一天同一会话合并为一份资料，后台分批核对，可能稍有延迟。
        </p>
        <p>
          已完成的历史范围每日复查，本轮进度可能变化。累计页数包含重放与复查，不代表唯一消息数。
          {counts?.last_progress_at &&
            ` 最近成功拉取：${time(counts.last_progress_at)}（北京时间）。`}
        </p>
      </details>
      {data ? (
        <HistoryTasks data={data} onChanged={() => setRetry((value) => value + 1)} />
      ) : (
        <p className="sync-empty" role="status">
          {error ? '暂时无法读取任务，页面会自动重试。' : '正在读取历史任务…'}
        </p>
      )}
    </section>
  );
}
