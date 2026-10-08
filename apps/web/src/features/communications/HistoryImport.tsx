import { useState, type FormEvent } from 'react';
import { api } from '../../api';
import type { Snapshot } from './types';
import { HistorySourcePicker } from './HistorySourcePicker';
import { ChevronDown, Plus } from 'lucide-react';

// 近半年固定为含今天的 180 个北京时间自然日。
const RECENT_HALF_YEAR_DAYS = 180;

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
  const [open, setOpen] = useState(false);
  const [scope, setScope] = useState<'all' | 'selected'>('all');
  const sources = data.sources.filter((source) => source.enabled && source.subscribed !== false);
  // 轮询后暂停或移除的来源不再参与提交，防止旧勾选悄悄恢复采集。
  const eligible = new Set(sources.filter((source) => selected.has(source.id)).map((s) => s.id));
  const [start, setStart] = useState(() => date(RECENT_HALF_YEAR_DAYS - 1));
  const [end, setEnd] = useState(() => date());
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState('');
  /** 全部与指定会话共用用户选择的日期，提交期间固定范围。 */
  async function enqueue() {
    if (busy) return;
    setBusy(true);
    setNotice('');
    const startDate = start;
    const endDate = end;
    try {
      const result = await api<{ count: number }>('/communications/history', {
        method: 'POST',
        body: JSON.stringify({
          selection:
            scope === 'all' ? { scope: 'all' } : { scope: 'selected', source_ids: [...eligible] },
          range: { start_date: startDate, end_date: endDate },
        }),
      });
      setNotice(
        result.count
          ? `已提交 ${result.count} 个会话（${startDate} 至 ${endDate}）。后台将逐步拉取、整理，关闭页面后继续；相同范围的进行中任务会继续原有进度。`
          : '当前没有可整理的已启用会话。',
      );
      setOpen(false);
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
  const count = scope === 'all' ? sources.length : eligible.size;
  return (
    <section className="sync-import">
      <div className="sync-heading">
        <div>
          <h2>导入历史消息</h2>
          <p>选择会话与日期，补充过去的沟通资料。</p>
        </div>
        <button
          className="sync-primary"
          aria-expanded={open}
          aria-controls="history-import-form"
          onClick={() => setOpen(!open)}
          disabled={busy}
        >
          {open ? <ChevronDown size={15} /> : <Plus size={15} />}
          {open ? '收起设置' : '新建导入'}
        </button>
      </div>
      {notice && (
        <p className="sync-notice" role="status">
          {notice}
        </p>
      )}
      {open && (
        <form id="history-import-form" onSubmit={submit}>
          <fieldset disabled={busy} className="sync-import-fields">
            <legend className="sr-only">导入范围</legend>
            <div className="sync-field-heading">1. 选择会话</div>
            <div className="sync-scope" role="group" aria-label="导入会话范围">
              <button type="button" aria-pressed={scope === 'all'} onClick={() => setScope('all')}>
                全部启用会话 · {sources.length}
              </button>
              <button
                type="button"
                aria-pressed={scope === 'selected'}
                onClick={() => setScope('selected')}
              >
                指定会话{eligible.size ? ` · ${eligible.size}` : ''}
              </button>
            </div>
            {scope === 'selected' && (
              <HistorySourcePicker sources={sources} selected={eligible} onChange={setSelected} />
            )}
            <p className="sync-note">只导入已订阅且启用的会话，暂停或已移除的会话不参与。</p>
            <div className="sync-field-heading">
              2. 选择日期 <small>北京时间</small>
            </div>
            <div className="sync-presets" role="group" aria-label="快捷日期">
              {[7, 30, 90, RECENT_HALF_YEAR_DAYS].map((days) => (
                <button
                  type="button"
                  key={days}
                  aria-pressed={start === date(days - 1) && end === date()}
                  onClick={() => {
                    setStart(date(days - 1));
                    setEnd(date());
                  }}
                >
                  近 {days} 天
                </button>
              ))}
            </div>
            <div className="sync-dates">
              <label>
                开始日期
                <input
                  type="date"
                  required
                  value={start}
                  max={end || date()}
                  onChange={(event) => setStart(event.target.value)}
                />
              </label>
              <span aria-hidden="true">—</span>
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
            <div className="sync-submit">
              <p>
                将导入 <strong>{count}</strong> 个会话 · {start || '未选日期'} 至{' '}
                {end || '未选日期'}
                <small>关闭页面后继续处理，相同日期的资料会合并更新。</small>
              </p>
              <button
                className="sync-primary"
                disabled={busy || !count || !start || !end || start > end || end > date()}
              >
                {busy ? '提交中…' : '开始导入'}
              </button>
            </div>
          </fieldset>
        </form>
      )}
    </section>
  );
}
