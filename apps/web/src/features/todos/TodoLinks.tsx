import { useEffect, useState, type FormEvent } from 'react';
import { api } from '../../api';
import type { Detail } from './types';

/** 按名称选择已有资源，Linear 使用可读编号；提交时仍由服务端重新验证。 */
export function LinksPanel({
  detail,
  saved,
  report,
}: {
  detail: Detail;
  saved: () => Promise<void>;
  report: (error: unknown) => void;
}) {
  const [kind, setKind] = useState('linear');
  const [resource, setResource] = useState('');
  const [query, setQuery] = useState('');
  const [options, setOptions] = useState<{ kind: string; id: string; label: string }[]>([]);
  useEffect(() => {
    let current = true;
    if (kind !== 'linear')
      void api<{ kind: string; id: string; label: string }[]>(
        `/todos/resources?q=${encodeURIComponent(query)}`,
      )
        .then((items) => {
          if (current) setOptions(items);
        })
        .catch(report);
    return () => {
      current = false;
    };
  }, [kind, query, report]);
  const [busy, setBusy] = useState(false);
  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    try {
      await api(`/todos/${detail.item.id}/links`, {
        method: 'POST',
        body: JSON.stringify({
          version: detail.item.version,
          kind,
          resource_id: resource,
          label:
            kind === 'linear'
              ? resource
              : (options.find((item) => item.id === resource)?.label ?? resource),
        }),
      });
      setResource('');
      await saved();
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  }
  return (
    <section className="todo-links">
      <p className="field-note">把相关会话、资料或 Linear 事项放在这里。</p>
      {detail.links.map((link) => (
        <p key={link.id}>
          {
            (
              {
                linear: 'Linear',
                conversation: '本人会话',
                communication: '沟通资料',
                knowledge: '知识条目',
              } as Record<string, string>
            )[link.kind]
          }{' '}
          · {link.label || '来源会话'}
        </p>
      ))}
      <form className="todo-link-form" onSubmit={(event) => void submit(event)}>
        <label>
          来源
          <select
            value={kind}
            onChange={(e) => {
              setKind(e.target.value);
              setResource('');
            }}
          >
            <option value="linear">Linear issue</option>
            <option value="conversation">本人会话</option>
            <option value="communication">沟通资料</option>
            <option value="knowledge">知识条目</option>
          </select>
        </label>
        {kind === 'linear' ? (
          <label>
            Linear 编号
            <input
              required
              maxLength={200}
              value={resource}
              onChange={(e) => setResource(e.target.value)}
              placeholder="例如 ENG-123"
            />
          </label>
        ) : (
          <>
            <label>
              搜索资料
              <input
                type="search"
                maxLength={500}
                value={query}
                onChange={(e) => {
                  setQuery(e.target.value);
                  setResource('');
                }}
                placeholder="会话名称、日期或知识标题"
              />
            </label>
            <label>
              选择资料
              <select required value={resource} onChange={(e) => setResource(e.target.value)}>
                <option value="">请选择（最多显示 50 项）</option>
                {options
                  .filter((item) => item.kind === kind)
                  .map((item) => (
                    <option value={item.id} key={item.id}>
                      {item.label || '未命名会话'}
                    </option>
                  ))}
              </select>
            </label>
          </>
        )}
        <button className="secondary-button" disabled={busy}>
          关联
        </button>
      </form>
    </section>
  );
}
