import { useEffect, useRef, useState } from 'react';
import { api } from '../../api';
import type { Source } from './types';

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
  const subscribed = new Set(sources.map((s) => s.chat_id));
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
    <section className="settings-card communication-card">
      <h2>选择订阅会话</h2>
      <p>
        只同步你主动选择的会话。单聊保留全部消息，群聊仅提取你发送、明确 @你或直接回复你的内容。
      </p>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          void load();
        }}
      >
        <label className="subscription-switch">
          <input
            type="checkbox"
            checked={filterDates}
            disabled={busy || saving}
            onChange={(e) => setFilterDates(e.target.checked)}
          />
          仅查找日期内有消息的会话
        </label>
        {filterDates && (
          <div className="history-dates">
            <label>
              开始日期
              <input
                type="date"
                required
                value={start}
                max={end}
                disabled={busy || saving}
                onChange={(e) => setStart(e.target.value)}
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
                disabled={busy || saving}
                onChange={(e) => setEnd(e.target.value)}
              />
            </label>
          </div>
        )}
        <div className="source-actions">
          <button className="primary" disabled={busy || saving}>
            {busy ? `已检查 ${scanned} 个会话…` : '筛选会话'}
          </button>
          {busy && (
            <button type="button" onClick={() => controller.current?.abort()}>
              停止筛选
            </button>
          )}
        </div>
      </form>
      {applied && <p>当前结果：{applied} · 日期筛选不自动订阅，也不自动导入历史。</p>}
      <label>
        搜索候选会话
        <input
          type="search"
          value={query}
          placeholder="联系人或群聊名称"
          onChange={(e) => setQuery(e.target.value)}
        />
      </label>
      <div className="history-selection-actions">
        <button
          disabled={busy || saving || !eligible.length}
          onClick={() => setSelected(new Set([...selected, ...eligible.map((c) => c.chat_id)]))}
        >
          全选当前结果（{eligible.length}）
        </button>
        <button disabled={saving || !selected.size} onClick={() => setSelected(new Set())}>
          清空选择
        </button>
      </div>
      <div className="history-source-options" role="group" aria-label="选择订阅会话">
        {visible.map((chat) => (
          <label className="history-source-option" key={chat.chat_id}>
            <input
              type="checkbox"
              disabled={saving || subscribed.has(chat.chat_id) || chat.failed}
              checked={selected.has(chat.chat_id) || subscribed.has(chat.chat_id)}
              onChange={(e) => {
                const next = new Set(selected);
                if (e.target.checked) next.add(chat.chat_id);
                else next.delete(chat.chat_id);
                setSelected(next);
              }}
            />
            <span>
              {chat.name || chat.chat_id}
              {subscribed.has(chat.chat_id)
                ? ' · 已订阅（启停见下方）'
                : chat.failed
                  ? ' · 日期查询失败'
                  : ''}
            </span>
          </label>
        ))}
        {!visible.length && (
          <p>
            {busy
              ? '正在查找…'
              : applied
                ? '没有匹配的会话，可调整日期或关键词。'
                : '先筛选会话，再勾选需要订阅的联系人或群聊。'}
          </p>
        )}
      </div>
      <button
        className="primary"
        disabled={busy || saving || !selected.size}
        onClick={() => void subscribe()}
      >
        {saving ? '正在订阅…' : `订阅所选会话（${selected.size}）`}
      </button>
      {notice && <p role="status">{notice}</p>}
    </section>
  );
}
