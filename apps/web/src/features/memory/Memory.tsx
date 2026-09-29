import { useCallback, useEffect, useState, type FormEvent } from 'react';
import { Brain, Plus, Search, Trash2, Pencil, X } from 'lucide-react';
import { api } from '../../api';
import { Spinner } from '../../components/Feedback';

/** 服务端原文投影，删除墓碑不会返回给页面。 */
type Entry = {
  /** 文件的稳定标识。 */
  id: string;
  /** 可修正的事实主题。 */
  key: string;
  /** 常驻档案或按需召回类别。 */
  kind: 'profile' | 'project' | 'episode';
  /** 人类可编辑的事实正文。 */
  content: string;
  /** 服务端写入时刻。 */
  updated_at: string;
  /** 到期后停止召回。 */
  expires_at: string | null;
  /** 自动提取来源，为空时是手工记录。 */
  source_run: string | null;
};
/** 能力和队列状态用于区分已保存、等待提取和等待索引。 */
type Snapshot = {
  /** 服务端是否启用记忆。 */
  enabled: boolean;
  /** 是否配置语义检索。 */
  semantic?: boolean;
  /** 是否允许后台提取。 */
  auto_extract?: boolean;
  /** 尚未处理完成的抽取数。 */
  pending?: number;
  /** 达到重试上限的抽取数。 */
  failed?: number;
  /** 当前向量版本的行数，可能等待原文补建。 */
  indexed?: number;
  /** 当前查询可见的原文条目。 */
  entries: Entry[];
};
// 与服务端限长保持一致；类别名称集中映射，避免散落在组件中。
const LABELS = { profile: '个人偏好', project: '项目事实', episode: '值得记住的事' };
const EMPTY = { key: '', kind: 'profile' as Entry['kind'], content: '', expires_at: '' };

/** 记忆管理按当前身份隔离；写入成功后重新读取服务端原文。 */
export function Memory({ report }: { report: (error: unknown) => void }) {
  const [data, setData] = useState<Snapshot | null>(null);
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState('');
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState<string | null>(null);
  const [form, setForm] = useState(EMPTY);
  const [notice, setNotice] = useState('');
  const load = useCallback(async () => {
    setData(await api<Snapshot>(`/memories?q=${encodeURIComponent(filter)}`));
  }, [filter]);
  useEffect(() => {
    let cancelled = false;
    api<Snapshot>(`/memories?q=${encodeURIComponent(filter)}`)
      .then((next) => {
        if (!cancelled) setData(next);
      })
      .catch((error: unknown) => {
        if (!cancelled) report(error);
      });
    return () => {
      cancelled = true;
    };
  }, [filter, report]);
  /** 保留到期时间原有时区，不把浏览器本地时间冒充 UTC。 */
  function edit(entry: Entry) {
    setEditing(entry.id);
    setForm({
      key: entry.key,
      kind: entry.kind,
      content: entry.content,
      expires_at: entry.expires_at || '',
    });
    setNotice('');
  }
  /** 手工写入会使旧模型上下文失效，页面明示这一取舍。 */
  async function save(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    try {
      await api(`/memories${editing ? `/${editing}` : ''}`, {
        method: editing ? 'PUT' : 'POST',
        body: JSON.stringify({ ...form, expires_at: form.expires_at.trim() || null }),
      });
      setEditing(null);
      setForm(EMPTY);
      setNotice(
        data?.semantic
          ? '已保存。后续对话会使用新的记忆，语义索引将在后台更新。'
          : '已保存。后续对话会使用新的记忆。',
      );
      await load();
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  }
  /** 删除前说明历史边界变化，成功后不保留正文在编辑框中。 */
  async function forget(entry: Entry) {
    if (
      !window.confirm(
        `忘记「${entry.key}」？这会删除记忆原文和检索索引，并让当前身份的旧聊天不再进入模型上下文。聊天记录仍可查看，正在生成的回复会停止。`,
      )
    )
      return;
    setBusy(true);
    try {
      await api(`/memories/${entry.id}`, { method: 'DELETE' });
      if (editing === entry.id) {
        setEditing(null);
        setForm(EMPTY);
      }
      setNotice('已遗忘。旧聊天不会再自动提取为记忆；历史记录仍保留供你查看。');
      await load();
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  }
  if (!data)
    return (
      <div className="settings-page">
        <Spinner />
      </div>
    );
  return (
    <div className="settings-page memory-page">
      <div className="page-eyebrow">PERSONAL MEMORY</div>
      <h1>让下一次对话，接得上。</h1>
      <p className="page-description">
        查看 Orbit 保存的事实，修正已经改变的偏好，也可以让它忘记。这里仅包含当前身份的记忆。
      </p>
      {!data.enabled ? (
        <p>长期记忆尚未启用，请配置服务端 MEMORY_ENABLED。</p>
      ) : (
        <>
          <div className="memory-status" role="status">
            <span className="tag">
              {data.semantic ? 'BM25 + 语义检索' : 'BM25 · 未配置向量模型'}
            </span>
            <span>{data.auto_extract ? '完成对话后自动提取' : '仅手工记录'}</span>
            <span>
              待提取 {data.pending ?? 0} · 提取失败 {data.failed ?? 0}
              {data.semantic ? ` · 向量 ${data.indexed ?? 0}` : ''}
            </span>
            <button
              className="secondary-button"
              onClick={() => void load().catch(report)}
              disabled={busy}
            >
              刷新
            </button>
          </div>
          {notice && (
            <p className="memory-notice" role="status">
              {notice}
            </p>
          )}
          <div className="settings-grid">
            <section className="settings-card">
              <div className="card-title">
                <Brain size={19} />
                <h2>{editing ? '修正记忆' : '记录一件事'}</h2>
              </div>
              <form onSubmit={(event) => void save(event)}>
                <label htmlFor="memory-key">主题</label>
                <input
                  id="memory-key"
                  required
                  maxLength={120}
                  placeholder="例如 reply.style"
                  value={form.key}
                  onChange={(event) => setForm({ ...form, key: event.target.value })}
                />
                <label htmlFor="memory-kind">类型</label>
                <select
                  id="memory-kind"
                  value={form.kind}
                  onChange={(event) =>
                    setForm({ ...form, kind: event.target.value as Entry['kind'] })
                  }
                >
                  {Object.entries(LABELS).map(([value, label]) => (
                    <option key={value} value={value}>
                      {label}
                    </option>
                  ))}
                </select>
                <label htmlFor="memory-content">事实或偏好</label>
                <textarea
                  id="memory-content"
                  rows={6}
                  required
                  maxLength={2000}
                  placeholder="例如：日常交流喜欢简短，只有明确要求时才展开分析。"
                  value={form.content}
                  onChange={(event) => setForm({ ...form, content: event.target.value })}
                />
                <label htmlFor="memory-expiry">到期时间（可选，含时区）</label>
                <input
                  id="memory-expiry"
                  placeholder="2026-12-31T23:59:59+08:00"
                  value={form.expires_at}
                  onChange={(event) => setForm({ ...form, expires_at: event.target.value })}
                />
                <p className="field-note">
                  个人偏好每轮优先载入，其他资料按需检索。手工保存会停止当前身份的生成并重置旧聊天上下文，保留可查看的聊天记录。
                </p>
                <div className="memory-actions">
                  <button className="primary-button" disabled={busy}>
                    {busy ? <Spinner /> : <Plus size={16} />}保存
                  </button>
                  {editing && (
                    <button
                      type="button"
                      className="secondary-button"
                      disabled={busy}
                      onClick={() => {
                        setEditing(null);
                        setForm(EMPTY);
                      }}
                    >
                      <X size={16} />
                      取消编辑
                    </button>
                  )}
                </div>
              </form>
            </section>
            <section className="settings-card">
              <div className="card-title">
                <Search size={19} />
                <h2>已经记下的事</h2>
              </div>
              <form
                className="memory-search"
                onSubmit={(event) => {
                  event.preventDefault();
                  setFilter(query);
                }}
              >
                <input
                  aria-label="搜索记忆"
                  placeholder="搜索主题或描述…"
                  maxLength={2000}
                  value={query}
                  onChange={(event) => setQuery(event.target.value)}
                />
                <button className="secondary-button" disabled={busy}>
                  搜索
                </button>
              </form>
              <div className="memory-list">
                {!data.entries.length && (
                  <p className="field-note">
                    {filter
                      ? '没有找到相关记忆。'
                      : '还没有保存的记忆。可以手工添加，或在完成对话后刷新。'}
                  </p>
                )}
                {data.entries.map((entry) => (
                  <article className="memory-entry" key={entry.id}>
                    <div className="memory-entry-title">
                      <strong>{entry.key}</strong>
                      <span className="tag">{LABELS[entry.kind]}</span>
                    </div>
                    <p>{entry.content}</p>
                    <small>
                      {entry.source_run ? '来自已完成对话' : '手工记录'} ·{' '}
                      {new Date(entry.updated_at).toLocaleString()}
                      {entry.expires_at
                        ? ` · ${new Date(entry.expires_at).getTime() <= Date.now() ? '已到期' : '到期于'} ${new Date(entry.expires_at).toLocaleString()}`
                        : ''}
                    </small>
                    <div className="memory-actions">
                      <button
                        className="secondary-button"
                        disabled={busy}
                        onClick={() => edit(entry)}
                      >
                        <Pencil size={14} />
                        修正
                      </button>
                      <button
                        className="secondary-button"
                        disabled={busy}
                        onClick={() => void forget(entry)}
                      >
                        <Trash2 size={14} />
                        遗忘
                      </button>
                    </div>
                  </article>
                ))}
              </div>
            </section>
          </div>
        </>
      )}
    </div>
  );
}
