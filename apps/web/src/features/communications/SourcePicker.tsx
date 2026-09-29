import { useState, type FormEvent } from 'react';
import { api } from '../../api';

/** 候选会话只用于选择，不会因为加载列表而开始采集。 */
interface Chat {
  /** 飞书会话 ID。 */ chat_id: string;
  /** 群名或单聊名称。 */ name: string | null;
}
/** 提供按页选择和手动会话 ID 两种方式，权限最终由服务端验证。 */
export function SourcePicker({
  onAdded,
  report,
}: {
  onAdded: () => Promise<void>;
  report: (e: unknown) => void;
}) {
  const [chats, setChats] = useState<Chat[]>([]);
  const [cursor, setCursor] = useState('');
  const [more, setMore] = useState(true);
  const [chatId, setChatId] = useState('');
  const [label, setLabel] = useState('');
  const [busy, setBusy] = useState(false);
  /** 去重累积当前账号可见列表，不预选任何会话。 */
  async function load() {
    setBusy(true);
    try {
      const data = await api<{ items: Chat[]; has_more: boolean; page_token: string }>(
        `/communications/chats?page_token=${encodeURIComponent(cursor)}`,
      );
      setChats((previous) => [
        ...new Map([...previous, ...data.items].map((chat) => [chat.chat_id, chat])).values(),
      ]);
      setCursor(data.page_token || '');
      setMore(data.has_more);
    } catch (e) {
      report(e);
    } finally {
      setBusy(false);
    }
  }
  /** 手动补充订阅只从当前时刻开始，历史补录独立提交。 */
  async function add(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    try {
      await api('/communications/sources', {
        method: 'POST',
        body: JSON.stringify({ chat_id: chatId, label }),
      });
      setChatId('');
      setLabel('');
      await onAdded();
    } catch (e) {
      report(e);
    } finally {
      setBusy(false);
    }
  }
  return (
    <section className="settings-card communication-card">
      <h2>补充订阅会话</h2>
      <p>自动发现尚未找到的会话，可以在这里手动补充。从现在开始同步，旧消息请使用历史整理。</p>
      <button disabled={busy || !more} onClick={() => void load()}>
        {chats.length ? (more ? '加载更多会话' : '已加载全部可见会话') : '加载我的会话'}
      </button>
      {chats.length > 0 && (
        <label>
          选择联系人或群聊
          <select
            value={chatId}
            onChange={(e) => {
              setChatId(e.target.value);
              setLabel(chats.find((c) => c.chat_id === e.target.value)?.name || '');
            }}
          >
            <option value="">请选择…</option>
            {chats.map((chat) => (
              <option key={chat.chat_id} value={chat.chat_id}>
                {chat.name || chat.chat_id}
              </option>
            ))}
          </select>
        </label>
      )}
      <form onSubmit={(e) => void add(e)}>
        <label>
          会话 ID
          <input
            value={chatId}
            onChange={(e) => setChatId(e.target.value.trim())}
            placeholder="选择后自动填入，也可粘贴 oc_ 开头的 ID"
            required
            maxLength={128}
          />
        </label>
        <label>
          显示名称
          <input
            value={label}
            onChange={(e) => setLabel(e.target.value)}
            required
            maxLength={120}
            placeholder="例如：产品讨论 / 与小林的沟通"
          />
        </label>
        <button className="primary" disabled={busy}>
          订阅新消息
        </button>
      </form>
    </section>
  );
}
