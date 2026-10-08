import { useEffect, useRef, useState } from 'react';
import { api } from '../../api';
import type { Source } from './types';
import { Search } from 'lucide-react';
import './source-picker.css';

// 候选分页与日期探测均有界，失败会话不会被当成无消息静默隐藏。
const MAX_PAGES = 200;
const PROBE_CONCURRENCY = 3;
/** 飞书可见会话，日期探测状态与订阅状态相互独立。 */
interface Chat {
  /** 上游标识。 */ chat_id: string;
  /** 可读名称。 */ name: string | null;
  /** 日期范围内是否存在消息；未知时需要重试。 */ active?: boolean;
  /** 单个会话读取失败，不影响其他候选。 */ failed?: boolean;
}
/** 日期使用北京时间，和服务端历史范围保持一致。 */
function date(daysAgo = 0) {
  return new Intl.DateTimeFormat('en-CA', {
    timeZone: 'Asia/Shanghai',
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
  }).format(new Date(Date.now() - daysAgo * 86400000));
}
/** 搜索、日期筛选和跨分页多选不产生订阅；只有明确提交才开始采集。 */
export function SourcePicker({
  onAdded,
  report,
  sources,
}: {
  onAdded: () => Promise<void>;
  report: (e: unknown) => void;
  sources: Source[];
}) {
  const [chats, setChats] = useState<Chat[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [query, setQuery] = useState('');
  const [start, setStart] = useState(() => date(29));
  const [end, setEnd] = useState(() => date());
  const [filterDates, setFilterDates] = useState(true);
  const [busy, setBusy] = useState(false);
  const [saving, setSaving] = useState(false);
  const [notice, setNotice] = useState('');
  const [scanned, setScanned] = useState(0);
  const [applied, setApplied] = useState('');
  const controller = useRef<AbortController | null>(null);
  useEffect(() => () => controller.current?.abort(), []);
  const subscribed = new Set(sources.filter((s) => s.subscribed !== false).map((s) => s.chat_id));
  const visible = chats.filter(
    (c) =>
      c.active !== false &&
      (c.name || c.chat_id).toLocaleLowerCase().includes(query.trim().toLocaleLowerCase()),
  );
  const eligible = visible.filter((c) => !subscribed.has(c.chat_id) && !c.failed);
  /** 分页扫描全部候选，三路并发探测日期；取消后保留已核对结果并明确未完成。 */
  async function load() {
    if (filterDates && (Date.parse(end) - Date.parse(start)) / 86400000 >= 366) {
      setNotice('单次日期筛选最多 366 天，请缩小范围。');
      return;
    }
    controller.current?.abort();
    const request = new AbortController();
    controller.current = request;
    setBusy(true);
    setChats([]);
    setSelected(new Set());
    setScanned(0);
    setNotice('');
    setApplied(filterDates ? `${start} 至 ${end}（北京时间）` : '全部可见会话');
    let cursor = '';
    const seen = new Set<string>();
    const accumulated = new Map<string, Chat>();
    try {
      for (let page = 0; page < MAX_PAGES; page++) {
        const data = await api<{ items: Chat[]; has_more: boolean; page_token: string }>(
          `/communications/chats?page_token=${encodeURIComponent(cursor)}`,
          { signal: request.signal },
        );
        for (let index = 0; index < data.items.length; index += PROBE_CONCURRENCY) {
          const batch = await Promise.all(
            data.items.slice(index, index + PROBE_CONCURRENCY).map(async (chat) => {
              if (!filterDates) return chat;
              try {
                const result = await api<{ active: boolean }>('/communications/chats/activity', {
                  method: 'POST',
                  signal: request.signal,
                  body: JSON.stringify({
                    chat_id: chat.chat_id,
                    range: { start_date: start, end_date: end },
                  }),
                });
                return { ...chat, active: result.active };
              } catch (error) {
                if (request.signal.aborted) throw error;
                return { ...chat, failed: true };
              }
            }),
          );
          if (request.signal.aborted) return;
          for (const chat of batch) accumulated.set(chat.chat_id, chat);
          setChats([...accumulated.values()]);
          setScanned(accumulated.size);
        }
        if (!data.has_more) {
          const failed = [...accumulated.values()].filter((c) => c.failed).length;
          setNotice(
            failed
              ? `筛选完成，${failed} 个会话读取失败，可重新筛选。`
              : '筛选完成。勾选会话后确认订阅。',
          );
          return;
        }
        if (!data.page_token || seen.has(data.page_token))
          throw new Error('会话分页异常，请重新筛选');
        seen.add(data.page_token);
        cursor = data.page_token;
      }
      setNotice('会话数量超出本次扫描上限，当前仅显示已检查的候选。');
    } catch (error) {
      if (request.signal.aborted) setNotice('已停止筛选，当前显示已检查的候选。');
      else {
        setNotice('筛选未完成，已保留当前结果，请重试。');
        report(error);
      }
    } finally {
      if (controller.current === request) setBusy(false);
    }
  }
  /** 各会话独立提交，部分失败保留对应勾选，重试不重复添加成功项。 */
  async function subscribe() {
    setSaving(true);
    setNotice('');
    const pending = chats.filter(
      (c) => selected.has(c.chat_id) && !subscribed.has(c.chat_id) && !c.failed,
    );
    const failed = new Set<string>();
    for (const chat of pending) {
      try {
        await api('/communications/sources', {
          method: 'POST',
          body: JSON.stringify({
            chat_id: chat.chat_id,
            label: (chat.name || chat.chat_id).slice(0, 120),
          }),
        });
      } catch {
        failed.add(chat.chat_id);
      }
    }
    setSelected(failed);
    setNotice(
      `已订阅 ${pending.length - failed.size} 个会话。${failed.size ? `${failed.size} 个失败，保留勾选以便重试。` : '从现在同步新消息；之前的消息请到「历史导入与进度」选择日期导入。'}`,
    );
    try {
      await onAdded();
    } catch (error) {
      report(error);
    } finally {
      setSaving(false);
    }
  }
  return (
    <section className="source-picker" aria-labelledby="source-picker-title">
      <div className="source-picker-heading">
        <h3 id="source-picker-title">选择订阅会话</h3>
        <details>
          <summary>同步范围说明</summary>
          <p>
            只同步你主动选择的会话。单聊保留全部消息，群聊仅提取你发送、明确
            @你或直接回复你的内容。日期仅用于查找会话，订阅后从现在开始同步；历史消息需单独导入。
          </p>
        </details>
      </div>
      <form
        className="source-picker-filter"
        onSubmit={(event) => {
          event.preventDefault();
          void load();
        }}
      >
        <label className="source-picker-date-toggle">
          <input
            type="checkbox"
            checked={filterDates}
            disabled={busy || saving}
            onChange={(event) => setFilterDates(event.target.checked)}
          />
          仅查找日期内有消息的会话
        </label>
        <div className="source-picker-filter-controls">
          {filterDates && (
            <div className="source-picker-dates">
              <input
                type="date"
                aria-label="开始日期"
                title="开始日期（北京时间）"
                required
                value={start}
                max={end}
                disabled={busy || saving}
                onChange={(event) => setStart(event.target.value)}
              />
              <span aria-hidden="true">—</span>
              <input
                type="date"
                aria-label="结束日期"
                title="结束日期（北京时间）"
                required
                value={end}
                min={start}
                max={date()}
                disabled={busy || saving}
                onChange={(event) => setEnd(event.target.value)}
              />
            </div>
          )}
          <button className="source-picker-filter-button" disabled={busy || saving}>
            {busy ? `已检查 ${scanned} 个…` : '筛选会话'}
          </button>
          {busy && (
            <button type="button" onClick={() => controller.current?.abort()}>
              停止
            </button>
          )}
        </div>
      </form>
      {applied && <p className="source-picker-applied">当前结果：{applied}</p>}
      <div className="source-picker-toolbar">
        <label className="source-picker-search">
          <Search size={14} />
          <input
            type="search"
            aria-label="搜索候选会话"
            value={query}
            placeholder="搜索联系人或群聊"
            onChange={(event) => setQuery(event.target.value)}
          />
        </label>
        <div className="source-picker-selection">
          <button
            disabled={busy || saving || !eligible.length}
            onClick={() =>
              setSelected(new Set([...selected, ...eligible.map((chat) => chat.chat_id)]))
            }
          >
            全选结果{eligible.length > 0 ? ` (${eligible.length})` : ''}
          </button>
          <button disabled={saving || !selected.size} onClick={() => setSelected(new Set())}>
            清空
          </button>
        </div>
      </div>
      <div
        className="source-picker-options"
        role="group"
        aria-label="选择订阅会话"
        aria-busy={busy}
      >
        {visible.map((chat) => (
          <label className="source-picker-option" key={chat.chat_id}>
            <input
              type="checkbox"
              disabled={saving || subscribed.has(chat.chat_id) || chat.failed}
              checked={selected.has(chat.chat_id) || subscribed.has(chat.chat_id)}
              onChange={(event) => {
                const next = new Set(selected);
                if (event.target.checked) next.add(chat.chat_id);
                else next.delete(chat.chat_id);
                setSelected(next);
              }}
            />
            <span className="source-picker-name" title={chat.name || chat.chat_id}>
              {chat.name || chat.chat_id}
            </span>
            {subscribed.has(chat.chat_id) ? (
              <small>已订阅</small>
            ) : chat.failed ? (
              <small className="source-picker-failed">查询失败</small>
            ) : null}
          </label>
        ))}
        {!visible.length && (
          <p className="source-picker-empty">
            {busy
              ? '正在查找会话…'
              : applied
                ? '没有匹配的会话，试试调整日期或关键词。'
                : '先筛选会话，再勾选需要订阅的联系人或群聊。'}
          </p>
        )}
      </div>
      <div className="source-picker-footer">
        <span>
          已选 <strong>{selected.size}</strong> 个
          <span className="source-picker-result-count"> · 当前显示 {visible.length} 个会话</span>
        </span>
        <button
          className="source-picker-submit"
          disabled={busy || saving || !selected.size}
          onClick={() => void subscribe()}
        >
          {saving ? '正在订阅…' : `订阅所选${selected.size ? ` (${selected.size})` : ''}`}
        </button>
      </div>
      {notice && (
        <p className="source-picker-notice" role="status">
          {notice}
        </p>
      )}
    </section>
  );
}
