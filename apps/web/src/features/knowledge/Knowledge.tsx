import { useCallback, useEffect, useState } from 'react';
import { BookOpen, Plus, RefreshCw } from 'lucide-react';
import { Link } from 'react-router-dom';
import { api } from '../../api';
import { Spinner } from '../../components/Feedback';
import { KnowledgeEditor } from './KnowledgeEditor';
import { KNOWLEDGE_STATUS, type KnowledgeEntry, type KnowledgeSnapshot } from './types';
import './knowledge.css';

/** 本人审批入口，候选默认不可对外使用；分页查询始终以服务端结果为准。 */
export function Knowledge({ report }: { report: (error: unknown) => void }) {
  const [data, setData] = useState<KnowledgeSnapshot | null>(null);
  const [status, setStatus] = useState('candidate');
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState('');
  const [offset, setOffset] = useState(0);
  const [editing, setEditing] = useState<KnowledgeEntry | null | undefined>(undefined);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState('');
  const path = `/knowledge?status=${status}&q=${encodeURIComponent(filter)}&offset=${offset}`;
  const load = useCallback(async () => {
    setData(await api<KnowledgeSnapshot>(path));
  }, [path]);
  useEffect(() => {
    let active = true;
    api<KnowledgeSnapshot>(path)
      .then((next) => {
        if (active) setData(next);
      })
      .catch(report);
    return () => {
      active = false;
    };
  }, [path, report]);
  /** 编辑后的正文随明确发布选择保存，不沿用条目此前的发布状态。 */
  async function save(title: string, content: string, publish: boolean, tags: string[]) {
    setBusy(true);
    try {
      await api(`/knowledge${editing ? `/${editing.id}` : ''}`, {
        method: editing ? 'PUT' : 'POST',
        body: JSON.stringify({
          title,
          content,
          tags,
          status: publish ? 'published' : 'candidate',
          version: editing?.version,
        }),
      });
      setEditing(undefined);
      setNotice(
        publish ? '已发布。后续他人问答和接管可以使用这段正文。' : '已保存为候选，暂不对外使用。',
      );
      await load();
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  }
  /** 撤回保留审阅记录；拒绝的候选不会在同一轮提取重试时自动恢复。 */
  async function change(entry: KnowledgeEntry, next: 'rejected' | 'revoked') {
    setBusy(true);
    try {
      await api(`/knowledge/${entry.id}`, {
        method: 'PUT',
        body: JSON.stringify({
          title: entry.title,
          content: entry.content,
          tags: entry.tags,
          version: entry.version,
          status: next,
        }),
      });
      setNotice(next === 'revoked' ? '已撤回，后续回复停止使用这条知识。' : '已拒绝这条候选。');
      if (editing?.id === entry.id) setEditing(undefined);
      await load();
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  }
  /** 仅显式删除当前条目，不恢复已发送消息或更改原始沟通资料。 */
  async function remove(entry: KnowledgeEntry) {
    if (!window.confirm(`删除「${entry.title}」？该知识将被删除，独立原始快照和沟通资料仍保留。`))
      return;
    setBusy(true);
    try {
      await api(`/knowledge/${entry.id}`, { method: 'DELETE' });
      if (editing?.id === entry.id) setEditing(undefined);
      await load();
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  }
  /** 失败队列重排不会再次提取已经成功的分块。 */
  async function retry() {
    setBusy(true);
    try {
      await api('/knowledge/retry', { method: 'POST' });
      await load();
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="settings-page memory-page knowledge-page">
      <div className="page-eyebrow">SHARED KNOWLEDGE</div>
      <h1>把经验，变成可复用的答案。</h1>
      <p className="page-description">
        从已订阅的日常沟通中提取业务事实和流程。只有你确认发布的正文，才会用于他人问答和代你回复；个人记忆与原文证据保持独立。
      </p>
      <div className="memory-actions">
        <Link className="secondary-button" to="/communications/records">
          从沟通文件提取
        </Link>
        <button className="primary-button" disabled={busy} onClick={() => setEditing(null)}>
          <Plus size={16} />
          补充知识
        </button>
        <button
          className="secondary-button"
          disabled={busy}
          onClick={() => void load().catch(report)}
        >
          <RefreshCw size={16} />
          刷新
        </button>
      </div>
      {notice && (
        <p role="status" className="memory-notice">
          {notice}
        </p>
      )}
      {!data ? (
        <Spinner />
      ) : (
        <>
          <div className="memory-status">
            <span>
              {data.extraction_enabled
                ? '每天北京时间 09:00 整理昨天的订阅沟通 · 发布需你确认'
                : '沟通采集未启用 · 可手工补充知识'}
            </span>
            <span>
              已入队待提取 {data.jobs.pending} · 失败 {data.jobs.failed} · 超长消息跳过{' '}
              {data.jobs.skipped}
            </span>
            {data.rag && (
              <span>
                语义索引 {data.rag.embedded} / {data.rag.chunks} · 待重试 {data.rag.retrying} ·
                未就绪时使用关键词检索
              </span>
            )}
            {data.jobs.failed > 0 && (
              <button className="secondary-button" disabled={busy} onClick={() => void retry()}>
                重试失败提取
              </button>
            )}
          </div>
          <div className="settings-grid">
            <section className="settings-card">
              <div className="card-title">
                <BookOpen size={19} />
                <h2>知识库</h2>
                <span className="tag">{data.total} 条</span>
              </div>
              <form
                className="knowledge-filters"
                onSubmit={(event) => {
                  event.preventDefault();
                  setFilter(query);
                  setOffset(0);
                }}
              >
                <select
                  aria-label="知识状态"
                  value={status}
                  onChange={(event) => {
                    setStatus(event.target.value);
                    setOffset(0);
                  }}
                >
                  <option value="">全部状态</option>
                  {Object.entries(KNOWLEDGE_STATUS).map(([value, label]) => (
                    <option key={value} value={value}>
                      {label}
                    </option>
                  ))}
                </select>
                <input
                  aria-label="搜索知识"
                  placeholder="搜索主题或答案"
                  maxLength={200}
                  value={query}
                  onChange={(event) => setQuery(event.target.value)}
                />
                <button className="secondary-button">搜索</button>
              </form>
              <div className="memory-list">
                {!data.items.length && (
                  <p className="field-note">
                    当前没有匹配的知识。候选会在每日扫描后生成，也可以手工补充。
                  </p>
                )}
                {data.items.map((entry) => (
                  <article className="memory-entry" key={entry.id}>
                    <div className="memory-entry-title">
                      <strong>{entry.title}</strong>
                      <span className="tag">{KNOWLEDGE_STATUS[entry.status]}</span>
                    </div>
                    <p>{entry.content}</p>
                    {entry.tags?.length > 0 && (
                      <p className="field-note">{entry.tags.join(' · ')}</p>
                    )}
                    <small>
                      {entry.extraction_day ? `${entry.extraction_day} 沟通提取` : '本人手工录入'} ·{' '}
                      {new Date(entry.updated_at).toLocaleString()}
                    </small>
                    <div className="memory-actions">
                      <button
                        className="secondary-button"
                        disabled={busy}
                        onClick={() => setEditing(entry)}
                      >
                        审阅与编辑
                      </button>
                      {entry.status === 'published' ? (
                        <button
                          className="secondary-button"
                          disabled={busy}
                          onClick={() => void change(entry, 'revoked')}
                        >
                          撤回发布
                        </button>
                      ) : (
                        entry.status === 'candidate' && (
                          <button
                            className="secondary-button"
                            disabled={busy}
                            onClick={() => void change(entry, 'rejected')}
                          >
                            拒绝
                          </button>
                        )
                      )}
                      <button
                        className="secondary-button"
                        disabled={busy}
                        onClick={() => void remove(entry)}
                      >
                        删除
                      </button>
                    </div>
                  </article>
                ))}
              </div>
              <div className="memory-actions">
                <button
                  className="secondary-button"
                  disabled={offset === 0 || busy}
                  onClick={() => setOffset(Math.max(0, offset - 20))}
                >
                  上一页
                </button>
                <span>第 {Math.floor(offset / 20) + 1} 页</span>
                <button
                  className="secondary-button"
                  disabled={!data.has_more || busy}
                  onClick={() => setOffset(offset + 20)}
                >
                  下一页
                </button>
              </div>
            </section>
            {editing !== undefined ? (
              <KnowledgeEditor
                key={editing?.id ?? 'new'}
                entry={editing}
                busy={busy}
                save={save}
                close={() => setEditing(undefined)}
              />
            ) : (
              <section className="settings-card">
                <h2>先核对，再复用</h2>
                <p className="field-note">
                  选择一条候选，核对原文、适用条件和隐私范围。发布后，agent
                  只使用你确认的答案，不引用原始私聊。
                </p>
              </section>
            )}
          </div>
        </>
      )}
    </div>
  );
}
