import { useState, type FormEvent } from 'react';
import { api } from '../../api';
import type { Snapshot } from './types';
import { HistorySourcePicker } from './HistorySourcePicker';

// 近一年固定为含今天的 365 个北京时间自然日。
const RECENT_YEAR_DAYS = 365;

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
  onQueued,
}: {
  data: Snapshot;
  reload: () => Promise<void>;
  report: (error: unknown) => void;
  onQueued: () => void;
}) {
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const sources = data.sources.filter((source) => source.enabled);
  // 轮询后暂停或移除的来源不再参与提交，防止旧勾选悄悄恢复采集。
  const eligible = new Set(sources.filter((source) => selected.has(source.id)).map((s) => s.id));
  const [start, setStart] = useState(() => date(RECENT_YEAR_DAYS - 1));
  const [end, setEnd] = useState(() => date());
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState('');
  /** 全部入口始终使用近一年；多选入口使用用户当前日期。 */
  async function enqueue(all = false) {
    if (busy) return;
    setBusy(true);
    setNotice('');
    const startDate = all ? date(RECENT_YEAR_DAYS - 1) : start;
    const endDate = all ? date() : end;
    try {
      const result = await api<{ count: number }>('/communications/history', {
        method: 'POST',
        body: JSON.stringify({
          selection: all ? { scope: 'all' } : { scope: 'selected', source_ids: [...eligible] },
          range: { start_date: startDate, end_date: endDate },
        }),
      });
      setNotice(
        result.count
          ? `已提交 ${result.count} 个会话（${startDate} 至 ${endDate}）。后台将逐步拉取、整理，关闭页面后继续；相同范围的进行中任务会继续原有进度。`
          : '当前没有可整理的已启用会话。',
      );
      onQueued();
      await reload();
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  }
  function submit(event: FormEvent) {
    event.preventDefault();
    void enqueue();
  }
  return (
    <section className="settings-card communication-card">
      <h2>整理历史消息</h2>
      <p>
        默认近一年（含今天的 365
        天），也可多选会话、自定日期。日期按北京时间划分；同一天的资料会更新，局部补录不会删除其他消息。
      </p>
      <div className="history-shortcut">
        <div>
          <strong>一次整理全部会话</strong>
          <p>包含当前已订阅且启用的 {sources.length} 个会话，不包含暂停或已移除的会话。</p>
        </div>
        <button
          type="button"
          className="primary"
          disabled={busy || !sources.length}
          onClick={() => void enqueue(true)}
        >
          {busy ? '提交中…' : '整理全部会话近一年'}
        </button>
      </div>
      <form onSubmit={submit}>
        <fieldset className="history-fields" disabled={busy}>
          <legend>选择会话与日期</legend>
          <HistorySourcePicker sources={sources} selected={eligible} onChange={setSelected} />
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
          <button className="primary-button" disabled={busy || !eligible.size}>
            {busy ? '提交中…' : `整理所选会话${eligible.size ? `（${eligible.size}）` : ''}`}
          </button>
        </fieldset>
      </form>
      {notice && <p role="status">{notice}</p>}
    </section>
  );
}
