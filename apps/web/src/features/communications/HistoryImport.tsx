import { useState, type FormEvent } from 'react';
import { api } from '../../api';
import type { Snapshot } from './types';

/** 日期选择与服务端统一按北京时间解释，不依赖访问设备的时区。 */
function date(daysAgo = 0) {
  return new Intl.DateTimeFormat('en-CA', {
    timeZone: 'Asia/Shanghai',
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
  }).format(new Date(Date.now() - daysAgo * 86400000));
}
/** 用户明确选择历史范围；补录更新同一日资料，不创建重复副本。 */
export function HistoryImport({
  data,
  reload,
  report,
}: {
  data: Snapshot;
  reload: () => Promise<void>;
  report: (error: unknown) => void;
}) {
  const [source, setSource] = useState('');
  const [start, setStart] = useState(date(7));
  const [end, setEnd] = useState(date());
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState('');
  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setNotice('');
    try {
      await api(`/communications/sources/${source}/history`, {
        method: 'POST',
        body: JSON.stringify({ start_date: start, end_date: end }),
      });
      setNotice('已加入历史队列。新消息继续同步，同一天的资料会合并更新。');
      await reload();
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  }
  const states: Record<string, string> = {
    pending: '等待拉取',
    running: '正在拉取',
    complete: '消息已拉取',
    failed: '拉取失败，将自动重试',
  };
  return (
    <section className="settings-card communication-card">
      <h2>整理历史消息</h2>
      <p>
        按需选择会话和日期，单次最多一年。日期按北京时间划分；当天已有资料会更新，局部补录不会删除其他消息。
      </p>
      <form onSubmit={(event) => void submit(event)}>
        <label>
          会话
          <select required value={source} onChange={(event) => setSource(event.target.value)}>
            <option value="">选择已订阅会话…</option>
            {data.sources
              .filter((s) => s.enabled)
              .map((s) => (
                <option key={s.id} value={s.id}>
                  {s.label}
                </option>
              ))}
          </select>
        </label>
        <div className="history-dates">
          <label>
            开始日期
            <input
              type="date"
              required
              value={start}
              max={end}
              onChange={(event) => setStart(event.target.value)}
            />
          </label>
          <label>
            结束日期
            <input
              type="date"
              required
              value={end}
              min={start}
              max={date()}
              onChange={(event) => setEnd(event.target.value)}
            />
          </label>
        </div>
        <button className="primary-button" disabled={busy || !source}>
          {busy ? '提交中…' : '开始整理历史'}
        </button>
        {notice && <p role="status">{notice}</p>}
      </form>
      {data.history_jobs.length > 0 && (
        <details className="history-jobs">
          <summary>历史任务 · {data.history_jobs.length}</summary>
          <p>“消息已拉取”之后仍可能在整理图片、摘要和索引，完整进度见会话列表。</p>
          {data.history_jobs.map((job) => (
            <div className="communication-evidence" key={job.id}>
              <strong>{data.sources.find((s) => s.id === job.source_id)?.label || '会话'}</strong>
              <p>
                {new Date(job.start_at * 1000).toLocaleDateString('zh-CN', {
                  timeZone: 'Asia/Shanghai',
                })}
                —
                {new Date((job.end_at - 1) * 1000).toLocaleDateString('zh-CN', {
                  timeZone: 'Asia/Shanghai',
                })}{' '}
                · {states[job.status] || job.status}
                {data.sources.find((s) => s.id === job.source_id)?.enabled === false &&
                job.status !== 'complete'
                  ? ' · 会话已暂停'
                  : ''}
              </p>
            </div>
          ))}
        </details>
      )}
    </section>
  );
}
