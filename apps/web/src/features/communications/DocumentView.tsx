import { useCallback, useEffect, useState, type FormEvent } from 'react';
import { api } from '../../api';
import type { Detail, Item } from './types';

// 分类展示不暗示机器摘要已被用户确认。
const LABELS: Record<string, string> = {
  decision: '决定',
  my_commitment: '我的承诺',
  their_commitment: '对方的承诺',
  open_question: '待确认',
  fact_candidate: '待确认的长期信息',
};
/** 原文和摘要并列核对，只有显式提交表单才创建提醒。 */
export function DocumentView({
  id,
  report,
  onClose,
}: {
  id: string;
  report: (e: unknown) => void;
  onClose: () => void;
}) {
  const [detail, setDetail] = useState<Detail | null>(null);
  const [offset, setOffset] = useState(0);
  const [item, setItem] = useState<number | null>(null);
  const [topic, setTopic] = useState('');
  const [due, setDue] = useState('');
  const [kind, setKind] = useState('reminder');
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState('');
  const [key, setKey] = useState(() => crypto.randomUUID());
  const load = useCallback(
    async () => setDetail(await api<Detail>(`/communications/documents/${id}?offset=${offset}`)),
    [id, offset],
  );
  useEffect(() => {
    void load().catch(report);
  }, [load, report]);
  /** 归纳文本可修正；选择另一个条目后更换幂等键。 */
  function select(value: Item, index: number) {
    setItem(index);
    setTopic(value.text);
    setNotice('');
    setKey(crypto.randomUUID());
  }
  /** 时间转换为带偏移的绝对时刻，页面明确使用浏览器本地时区。 */
  async function create(event: FormEvent) {
    event.preventDefault();
    if (item === null || !detail) return;
    setBusy(true);
    try {
      await api('/followups', {
        method: 'POST',
        body: JSON.stringify({
          idempotency_key: key,
          kind,
          topic,
          due_at: new Date(due).toISOString(),
          communication: { document_id: id, version: detail.document.version, item },
        }),
      });
      setNotice('已加入提醒与跟进。来源被修正、暂停或遗忘时，此事项会停止。');
      setItem(null);
    } catch (e) {
      report(e);
    } finally {
      setBusy(false);
    }
  }
  return (
    <section className="settings-card communication-card">
      <div className="communication-row">
        <h2>沟通整理 · {detail?.document.day}</h2>
        <button onClick={onClose}>关闭资料</button>
      </div>
      <p>机器归纳需结合原话核对。长期信息候选不会自动写入个人记忆。</p>
      {notice && <p role="status">{notice}</p>}
      {!detail ? (
        <p>读取中…</p>
      ) : (
        <>
          {!detail.summary && (
            <p>
              等待整理；原始记录已经保存。
              <button onClick={() => void load().catch(report)}>刷新</button>
            </p>
          )}
          {detail.summary && (
            <p>
              共 {detail.summary.message_count} 条记录；{detail.summary.unsupported_count}{' '}
              条附件、无文本或超长消息未参与摘要。
            </p>
          )}
          {detail.summary?.items.length === 0 && <p>这部分资料没有提取到明确事项。</p>}
          {detail.summary?.items.map((value, index) => (
            <article className="communication-evidence" key={`${value.message_id}-${index}`}>
              <span className="tag">{LABELS[value.kind] || value.kind}</span>
              <p>{value.text}</p>
              <blockquote>{value.quote}</blockquote>
              <small>
                {value.is_me ? '我' : value.sender_id} ·{' '}
                {new Date(value.create_time).toLocaleString()} · {value.message_id}
              </small>
              <button onClick={() => select(value, index)}>据此安排提醒</button>
            </article>
          ))}
          {item !== null && (
            <form onSubmit={(event) => void create(event)}>
              <h3>确认跟进事项</h3>
              <label>
                要提醒我的事
                <input
                  value={topic}
                  maxLength={500}
                  onChange={(e) => {
                    setTopic(e.target.value);
                    setKey(crypto.randomUUID());
                  }}
                  required
                />
              </label>
              <label>
                方式
                <select
                  value={kind}
                  onChange={(e) => {
                    setKind(e.target.value);
                    setKey(crypto.randomUUID());
                  }}
                >
                  <option value="reminder">到时提醒</option>
                  <option value="checkin">轻量回访（需已开启主动回访）</option>
                </select>
              </label>
              <label>
                时间（{Intl.DateTimeFormat().resolvedOptions().timeZone}）
                <input
                  type="datetime-local"
                  value={due}
                  required
                  onChange={(e) => {
                    setDue(e.target.value);
                    setKey(crypto.randomUUID());
                  }}
                />
              </label>
              <button className="primary" disabled={busy}>
                确认安排
              </button>
              <button type="button" onClick={() => setItem(null)}>
                取消
              </button>
            </form>
          )}
          <h3>原始记录</h3>
          {detail.messages.map((message) => (
            <article key={message.message_id} className="communication-evidence">
              <small>
                {message.is_me ? '我' : message.sender_id || '未知发送者'} ·{' '}
                {new Date(message.create_time).toLocaleString()}
              </small>
              <p>
                {message.deleted
                  ? '消息已撤回'
                  : message.text || `［${message.message_type}：仅保留元数据］`}
              </p>
              <small>{message.message_id}</small>
            </article>
          ))}
          <div className="communication-row">
            <button disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - 50))}>
              上一页
            </button>
            <span>
              {detail.total ? offset + 1 : 0}–{Math.min(offset + 50, detail.total)} / {detail.total}
            </span>
            <button disabled={offset + 50 >= detail.total} onClick={() => setOffset(offset + 50)}>
              下一页
            </button>
          </div>
        </>
      )}
    </section>
  );
}
