import { useEffect, useRef, useState } from 'react';
import { ChevronDown, Search } from 'lucide-react';
import { api, errorText } from '../../api';
import type { Progress } from './HistoryProgress';

// 服务端只返回优先级最高的 20 项，筛选范围始终明确标为当前列表。
const STATES: Record<string, string> = {
  pending: '等待拉取',
  running: '正在拉取',
  complete: '本轮已拉取',
  failed: '失败重试',
  cancelled: '已取消',
  paused: '已暂停',
};
/** 取消状态优先于来源暂停，避免已取消任务被错误归类。 */
function status(job: Progress['jobs'][number]) {
  return job.status === 'cancelled' ? 'cancelled' : !job.enabled ? 'paused' : job.status;
}
/** 排他终点减一秒后显示为用户选择的最后一个北京时间自然日。 */
function date(seconds: number) {
  return new Date(seconds * 1000).toLocaleDateString('zh-CN', { timeZone: 'Asia/Shanghai' });
}
/** 任务采用紧凑行，只有展开时展示累计分页、重试时间和错误分类。 */
export function HistoryTasks({ data, onChanged }: { data: Progress; onChanged: () => void }) {
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState('all');
  const [target, setTarget] = useState<{ id?: string; label: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    if (!target) return;
    const previous = document.activeElement as HTMLElement | null;
    const modal = dialog.current;
    modal?.showModal();
    return () => {
      modal?.close();
      previous?.focus();
    };
  }, [target]);
  /** 确认后才写入取消请求；失败保留弹窗，成功刷新权威进度。 */
  async function cancel() {
    if (busy || !target) return;
    setBusy(true);
    setError('');
    try {
      const result = await api<{ count: number }>('/communications/history/cancel', {
        method: 'POST',
        body: JSON.stringify(target.id ? { scope: 'job', id: target.id } : { scope: 'all' }),
      });
      setNotice(`已取消 ${result.count} 个历史任务，已导入资料保留，新消息订阅继续同步。`);
      setTarget(null);
      onChanged();
    } catch (reason) {
      setError(errorText(reason));
    } finally {
      setBusy(false);
    }
  }
  const jobs = data.jobs.filter(
    (job) =>
      job.label.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase()) &&
      (filter === 'all' ||
        (filter === 'active'
          ? ['pending', 'running'].includes(status(job))
          : status(job) === filter)),
  );
  return (
    <div className="sync-tasks">
      <div className="sync-heading">
        <div>
          <h3>
            历史任务 <small>{data.counts.total}</small>
          </h3>
          <p>优先展示失败与进行中的任务，最多 20 项。</p>
        </div>
        <button
          className="sync-button"
          disabled={data.counts.total === data.counts.cancelled || busy}
          onClick={() => {
            setError('');
            setTarget({ label: '全部历史任务' });
          }}
        >
          取消全部任务
        </button>
      </div>
      {notice && (
        <p className="sync-notice" role="status">
          {notice}
        </p>
      )}
      {data.counts.total > 0 ? (
        <>
          <div className="sync-task-filters">
            <label className="sync-search">
              <Search size={15} />
              <input
                type="search"
                aria-label="搜索当前任务列表"
                placeholder="搜索当前列表中的会话…"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
              />
            </label>
            <select
              aria-label="筛选当前任务列表状态"
              value={filter}
              onChange={(event) => setFilter(event.target.value)}
            >
              <option value="all">全部状态</option>
              <option value="active">等待与进行中</option>
              <option value="failed">失败重试</option>
              <option value="complete">本轮已拉取</option>
              <option value="paused">已暂停</option>
              <option value="cancelled">已取消</option>
            </select>
            <span>
              显示 {jobs.length} / {data.jobs.length} 项
            </span>
          </div>
          <div className="sync-task-columns" aria-hidden="true">
            <span>会话 / 导入日期</span>
            <span>状态</span>
            <span>累计分页</span>
          </div>
          {jobs.map((job) => (
            <details className="sync-task" key={job.id}>
              <summary>
                <span className="sync-task-name">
                  <ChevronDown size={14} />
                  <span>
                    <strong>{job.label}</strong>
                    <small>
                      {date(job.start_at)} — {date(job.end_at - 1)}
                    </small>
                  </span>
                </span>
                <span
                  className={`sync-task-status ${status(job) === 'failed' ? 'error-text' : ''}`}
                >
                  <i />
                  {STATES[status(job)] || job.status}
                </span>
                <span className="sync-task-pages">{job.pages_processed} 页</span>
              </summary>
              <div className="sync-task-detail">
                <p>
                  {job.last_progress_at
                    ? `最近成功拉取：${new Date(job.last_progress_at).toLocaleString('zh-CN', { timeZone: 'Asia/Shanghai' })}（北京时间）`
                    : '暂无成功分页记录'}
                </p>
                {job.error && job.enabled && job.status !== 'cancelled' && (
                  <p className="error-text">
                    {job.error === 'communication_provider_rejected'
                      ? '飞书拒绝读取，请检查授权和消息权限。'
                      : '拉取暂时失败，后台会自动重试。'}{' '}
                    错误分类：{job.error}。最早重试时间：
                    {new Date(job.next_attempt).toLocaleString('zh-CN', {
                      timeZone: 'Asia/Shanghai',
                    })}
                    （北京时间）。
                  </p>
                )}
                {job.status === 'complete' && (
                  <p>本轮消息已拉取，资料继续整理；此范围仍会每日复查。</p>
                )}
                {job.status !== 'cancelled' && (
                  <button
                    className="sync-button"
                    disabled={busy}
                    onClick={() => {
                      setError('');
                      setTarget({ id: job.id, label: job.label });
                    }}
                  >
                    取消此任务
                  </button>
                )}
              </div>
            </details>
          ))}
          {!jobs.length && (
            <p className="sync-empty">
              当前列表中没有匹配任务。筛选仅作用于已展示的 {data.jobs.length} 项。
            </p>
          )}
        </>
      ) : (
        <div className="sync-empty">
          <strong>还没有历史导入任务</strong>
          <p>点击上方「新建导入」，选择会话和日期开始。</p>
        </div>
      )}
      {target && (
        <dialog
          ref={dialog}
          className="sync-dialog"
          aria-labelledby="cancel-history-title"
          aria-describedby="cancel-history-description"
          onCancel={(event) => {
            event.preventDefault();
            if (!busy) setTarget(null);
          }}
        >
          <h2 id="cancel-history-title">{target.id ? '取消此历史任务？' : '取消全部历史任务？'}</h2>
          <p id="cancel-history-description">
            {target.id
              ? `会话：${target.label}`
              : '取消全部历史任务，包含已暂停任务与已完成范围的每日复查，不受当前搜索或筛选影响。'}
          </p>
          <p>停止历史消息拉取与每日复查。已有资料保留并继续整理，新消息订阅不受影响。</p>
          <p>需要恢复时，可重新提交相同的会话和日期范围。</p>
          {error && (
            <p className="error-text" role="alert">
              {error}
            </p>
          )}
          <div className="sync-dialog-actions">
            <button
              className="sync-button"
              autoFocus
              disabled={busy}
              onClick={() => setTarget(null)}
            >
              返回
            </button>
            <button className="sync-primary" disabled={busy} onClick={() => void cancel()}>
              {busy ? '取消中…' : '确认取消任务'}
            </button>
          </div>
        </dialog>
      )}
    </div>
  );
}
