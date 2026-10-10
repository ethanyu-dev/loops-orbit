import { useState, type FormEvent } from 'react';
import { api } from '../../api';
import { contentOf, localTime, STATUS, type Todo } from './types';

/** 编辑草稿保存创建时的版本，冲突时保留内容；低频字段按需展开。 */
export function TodoEditor({
  item,
  saved,
  report,
  cancel,
}: {
  item?: Todo;
  saved: (id: string, message?: string) => Promise<void>;
  report: (error: unknown) => void;
  cancel: () => void;
}) {
  const [original, setOriginal] = useState(item);
  const [content, setContent] = useState(() => contentOf(item));
  const [status, setStatus] = useState(item?.status ?? 'active');
  const [key, setKey] = useState(() => crypto.randomUUID());
  const [busy, setBusy] = useState(false);
  function change(name: keyof typeof content, value: string | null) {
    setContent((old) => ({ ...old, [name]: value }));
    setKey(crypto.randomUUID());
  }
  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    try {
      const result = await api<{ id: string; delivery_may_have_started?: boolean }>(
        `/todos${original ? `/${original.id}` : ''}`,
        {
          method: original ? 'PUT' : 'POST',
          body: JSON.stringify(
            original
              ? { version: original.version, content, status }
              : { idempotency_key: key, content },
          ),
        },
      );
      await saved(
        result.id,
        result.delivery_may_have_started
          ? '已保存；已经开始投递的消息仍可能送达。'
          : original
            ? '待办已更新'
            : '待办已创建，可以继续添加安排。',
      );
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  }
  return (
    <section className="settings-card todo-editor">
      <div className="todo-section-heading">
        <h2>{original ? '编辑事项' : '新建待办'}</h2>
        <span className="todo-meta">{original ? '修改后保存' : '先记下来，稍后再安排'}</span>
      </div>
      <form onSubmit={(event) => void submit(event)}>
        {item && original && item.version !== original.version && (
          <div className="todo-draft-warning" role="status">
            <span>事项已有新修改，当前草稿已保留。</span>
            <button
              type="button"
              className="todo-text-button"
              onClick={() => {
                setOriginal(item);
                setContent(contentOf(item));
                setStatus(item.status);
              }}
            >
              用最新内容替换草稿
            </button>
          </div>
        )}
        <div className="todo-form-grid">
          <label className="todo-full">
            事情名称
            <input
              required
              autoFocus
              maxLength={500}
              value={content.title}
              onChange={(e) => change('title', e.target.value)}
              placeholder="想完成什么？"
            />
          </label>
          <label className="todo-full">
            下一步
            <input
              maxLength={4000}
              value={content.next_action}
              onChange={(e) => change('next_action', e.target.value)}
              placeholder="一个可以开始的具体行动（可选）"
            />
          </label>
          {original && (
            <label>
              状态
              <select value={status} onChange={(e) => setStatus(e.target.value)}>
                {Object.entries(STATUS).map(([value, label]) => (
                  <option key={value} value={value}>
                    {label}
                  </option>
                ))}
              </select>
            </label>
          )}
          <label className={original ? '' : 'todo-full'}>
            截止时间 <span className="todo-optional">可选 · 浏览器时区</span>
            <input
              type="datetime-local"
              value={
                content.due_at
                  ? localTime(content.due_at, Intl.DateTimeFormat().resolvedOptions().timeZone)
                  : ''
              }
              onChange={(e) =>
                change('due_at', e.target.value ? new Date(e.target.value).toISOString() : null)
              }
            />
          </label>
        </div>
        <details className="todo-disclosure">
          <summary>
            补充目标、完成标准与等待事项
            {(content.objective || content.completion_criteria || content.waiting_on) && (
              <span className="todo-meta"> · 已填写</span>
            )}
          </summary>
          <div className="todo-form-grid">
            {(['objective', 'completion_criteria', 'waiting_on'] as const).map((name, i) => (
              <label className={i === 0 ? 'todo-full' : ''} key={name}>
                {['目标', '怎样算完成', '在等什么 / 谁'][i]}
                <textarea
                  rows={2}
                  maxLength={4000}
                  value={content[name]}
                  onChange={(e) => change(name, e.target.value)}
                />
              </label>
            ))}
          </div>
        </details>
        {original && ['completed', 'cancelled'].includes(status) && (
          <p className="field-note">保存后停止此事项的全部安排；重新打开不会自动恢复。</p>
        )}
        <div className="todo-form-actions">
          <button className="primary-button" disabled={busy}>
            {busy ? '保存中…' : original ? '保存修改' : '创建待办'}
          </button>
          <button type="button" className="secondary-button" disabled={busy} onClick={cancel}>
            取消
          </button>
        </div>
      </form>
    </section>
  );
}
