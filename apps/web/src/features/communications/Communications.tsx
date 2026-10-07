import { useEffect, useState, type FormEvent } from 'react';
import { api } from '../../api';
import { NavLink, Navigate, useLocation, useNavigate, useSearchParams } from 'react-router-dom';
import { CONVERSATION_ID } from '../../layout/navigation';
import { HistoryProgress } from './HistoryProgress';
import { HistoryImport } from './HistoryImport';
import { SourcePicker } from './SourcePicker';
import { ConnectionCard } from './ConnectionCard';
import { Spinner } from '../../components/Feedback';
import { DocumentView } from './DocumentView';
import { useCommunicationSnapshot } from './useCommunicationSnapshot';
import type { Source, Document } from './types';

// 大量已订阅会话按页展示，避免一次渲染全部操作面板。
const SOURCE_PAGE_SIZE = 25;

/** 飞书资料的独立管理入口；授权、选择来源和删除分别是明确的用户动作。 */
export function Communications({ report }: { report: (e: unknown) => void }) {
  const { data, error: loadError, loading, load } = useCommunicationSnapshot();
  const [searchParams, setSearchParams] = useSearchParams();
  const navigate = useNavigate();
  const { pathname } = useLocation();
  const documentId = pathname.match(/^\/communications\/records\/([^/]+)$/)?.[1];
  const selected = documentId && CONVERSATION_ID.test(documentId) ? documentId : null;
  const section = pathname.startsWith('/communications/sources')
    ? 'sources'
    : pathname.startsWith('/communications/sync')
      ? 'sync'
      : 'records';
  const [historyRevision, setHistoryRevision] = useState(0);
  const [query, setQuery] = useState('');
  const [sourceQuery, setSourceQuery] = useState('');
  const [sourcePage, setSourcePage] = useState(0);
  const [results, setResults] = useState<Document[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirmation, setConfirmation] = useState<string | null>(null);
  const [outcome] = useState(() => new URLSearchParams(location.search).get('feishu'));
  useEffect(() => {
    if (!searchParams.has('feishu')) return;
    const next = new URLSearchParams(searchParams);
    next.delete('feishu');
    setSearchParams(next, { replace: true });
  }, [searchParams, setSearchParams]);
  /** 导航到飞书授权页，凭证交换始终在服务端完成。 */
  async function connect() {
    setBusy(true);
    try {
      const { url } = await api<{ url: string }>('/communications/oauth/start', { method: 'POST' });
      location.assign(url);
    } catch (e) {
      report(e);
      setBusy(false);
    }
  }
  /** 所有写操作完成后重新读取服务端，删除时同步清除详情和检索缓存。 */
  async function change(path: string, method: string, body?: unknown) {
    setBusy(true);
    try {
      await api(`/communications${path}`, {
        method,
        body: body === undefined ? undefined : JSON.stringify(body),
      });
      setConfirmation(null);
      setResults(null);
      await load();
    } catch (e) {
      report(e);
    } finally {
      setBusy(false);
    }
  }
  /** 空查询回到最近文档；非空查询与聊天使用同一 BM25 / 向量检索。 */
  async function find(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    try {
      setResults(
        query.trim()
          ? (
              await api<{ document: Document }[]>(
                `/communications/search?q=${encodeURIComponent(query)}`,
              )
            ).map((hit) => hit.document)
          : null,
      );
    } catch (e) {
      report(e);
    } finally {
      setBusy(false);
    }
  }
  /** 状态描述区分初次同步、分页进行中和上游权限失败。 */
  function status(source: Source) {
    return !source.enabled
      ? '已暂停采集和检索'
      : source.error
        ? '同步失败，请检查授权后重试'
        : source.window_end
          ? '正在分页同步…'
          : source.last_synced_at
            ? `消息拉取完成于 ${new Date(source.last_synced_at).toLocaleString()}`
            : '等待拉取新消息';
  }
  const withDocuments = new Set(data?.progress.filter((p) => p.total > 0).map((p) => p.source_id));
  const filteredSources = (data?.sources || [])
    .filter((s) => s.label.toLocaleLowerCase().includes(sourceQuery.trim().toLocaleLowerCase()))
    .sort(
      (a, b) =>
        Number(withDocuments.has(b.id)) - Number(withDocuments.has(a.id)) ||
        a.label.localeCompare(b.label, 'zh-CN'),
    );
  const page = Math.min(
    sourcePage,
    Math.max(0, Math.ceil(filteredSources.length / SOURCE_PAGE_SIZE) - 1),
  );
  const visibleSources = filteredSources.slice(
    page * SOURCE_PAGE_SIZE,
    (page + 1) * SOURCE_PAGE_SIZE,
  );
  const legacy = searchParams.get('communication');
  if (legacy && CONVERSATION_ID.test(legacy))
    return <Navigate replace to={`/communications/records/${legacy}`} />;
  if (pathname === '/communications')
    return <Navigate replace to={`/communications/records${location.search}`} />;
  if (selected)
    return (
      <DocumentView
        key={selected}
        id={selected}
        report={report}
        onClose={() => void navigate('/communications/records')}
      />
    );
  if (
    !['/communications/records', '/communications/sync', '/communications/sources'].includes(
      pathname,
    )
  )
    return (
      <div className="settings-page">
        <h1>页面不存在</h1>
        <NavLink to="/communications/records">返回沟通记录</NavLink>
      </div>
    );
  return (
    <div className="settings-page communication-page">
      <div className="communication-heading">
        <div>
          <h1>飞书沟通资料</h1>
          <p className="page-description">查找沟通记录，管理同步范围。</p>
        </div>
      </div>
      {outcome === 'failed' && (
        <p className="connection-error" role="alert">
          授权未完成。请确认账号在白名单内，应用权限和回调地址已配置，再重新连接。
        </p>
      )}
      {loadError && (
        <section className="settings-card communication-card" role="alert">
          <h2>{data ? '连接状态暂未更新' : '暂时无法读取连接状态'}</h2>
          <p>{loadError}</p>
          <p>
            {data ? '以下保留上次读取的内容。' : '这不代表飞书已断开或后台历史任务已停止。'}
            页面会自动重试。
          </p>
          <button disabled={loading} onClick={() => void load()}>
            {loading ? '正在重试…' : '重新读取'}
          </button>
        </section>
      )}
      {!data && loadError ? null : !data ? (
        <div className="loading-row">
          <Spinner />
          正在读取连接状态…
        </div>
      ) : !data.enabled ? (
        <section className="settings-card">
          <h2>尚未启用沟通采集</h2>
          <p>需要在服务端配置用户授权和本地凭证加密密钥。机器人聊天入口可继续独立使用。</p>
        </section>
      ) : (
        <>
          <ConnectionCard
            data={data}
            busy={busy}
            connect={connect}
            disconnect={() => setConfirmation('connection')}
            report={report}
          />
          {data.connection && (
            <>
              <nav className="communication-tabs" aria-label="飞书资料分区">
                <NavLink to="/communications/records">沟通记录</NavLink>
                <NavLink to="/communications/sync">历史导入与进度</NavLink>
                <NavLink to="/communications/sources">订阅管理 · {data.sources.length}</NavLink>
              </nav>
              {section === 'sources' && (
                <div>
                  <SourcePicker report={report} onAdded={load} sources={data.sources} />
                  <section className="settings-card communication-card">
                    <h2>已订阅会话 · {data.sources.length}</h2>
                    {!data.sources.length && <p>尚未订阅会话，请在上方勾选后订阅。</p>}
                    <label>
                      查找已订阅会话
                      <input
                        value={sourceQuery}
                        placeholder="输入联系人或群聊名称"
                        onChange={(e) => {
                          setSourceQuery(e.target.value);
                          setSourcePage(0);
                        }}
                      />
                    </label>
                    {visibleSources.map((source) => (
                      <article className="communication-evidence" key={source.id}>
                        <strong>{source.label}</strong>
                        <p>{status(source)}</p>
                        {data.progress
                          .filter((p) => p.source_id === source.id)
                          .map((p) => (
                            <div className="source-progress" key={p.source_id}>
                              <span>
                                文字资料可用 {p.ready}/{p.total} 天
                              </span>
                              {p.checking > 0 && <span>统计更新中 {p.checking} 天</span>}
                              {p.summarizing > 0 && <span>待整理 {p.summarizing} 天</span>}
                              {p.indexing > 0 && <span>索引处理中 {p.indexing} 天</span>}
                              {p.errors > 0 && (
                                <span className="error-text">整理失败 {p.errors} 天</span>
                              )}
                              {p.images > 0 && (
                                <span>
                                  图片解读 {p.images_ready}/{p.images}
                                  {p.images_failed > 0 ? ` · ${p.images_failed} 张失败重试中` : ''}
                                </span>
                              )}
                            </div>
                          ))}
                        <div className="source-actions">
                          <button
                            disabled={busy}
                            onClick={() =>
                              void change(`/sources/${source.id}`, 'PUT', {
                                version: source.version,
                                enabled: !source.enabled,
                              })
                            }
                          >
                            {source.enabled ? '暂停同步与检索' : '恢复同步与检索'}
                          </button>
                          <button
                            disabled={busy || !source.enabled}
                            onClick={() => void change(`/sources/${source.id}/sync`, 'POST')}
                          >
                            同步最新消息
                          </button>
                          <details className="source-more">
                            <summary>更多</summary>
                            <button
                              className="danger-action"
                              disabled={busy}
                              onClick={() => setConfirmation(source.id)}
                            >
                              移除会话并删除资料
                            </button>
                          </details>
                        </div>
                      </article>
                    ))}
                    {filteredSources.length === 0 && data.sources.length > 0 && (
                      <p>没有匹配的会话。</p>
                    )}
                    {filteredSources.length > SOURCE_PAGE_SIZE && (
                      <div className="communication-row">
                        <button disabled={page === 0} onClick={() => setSourcePage(page - 1)}>
                          上一页会话
                        </button>
                        <span>
                          第 {page + 1}/{Math.ceil(filteredSources.length / SOURCE_PAGE_SIZE)} 页 ·{' '}
                          {filteredSources.length} 个会话
                        </span>
                        <button
                          disabled={(page + 1) * SOURCE_PAGE_SIZE >= filteredSources.length}
                          onClick={() => setSourcePage(page + 1)}
                        >
                          下一页会话
                        </button>
                      </div>
                    )}
                  </section>
                </div>
              )}
              {section === 'sync' && (
                <div>
                  <HistoryProgress revision={historyRevision} documents={data.progress} />
                  <HistoryImport
                    data={data}
                    reload={load}
                    report={report}
                    onQueued={() => {
                      setHistoryRevision((value) => value + 1);
                      const panel = document.getElementById('history-progress');
                      panel?.focus({ preventScroll: true });
                      panel?.scrollIntoView({ behavior: 'smooth', block: 'start' });
                    }}
                  />
                </div>
              )}
              {confirmation && (
                <section className="settings-card communication-card" role="alert">
                  <h2>移除并删除资料？</h2>
                  <p>
                    会删除
                    {confirmation === 'connection'
                      ? '所有导入资料和本地授权凭证'
                      : '此会话的原文、图片解读、摘要和检索索引'}
                    ，并停止依赖这些资料的提醒。飞书中的原始消息不受影响。已发送的聊天记录仍可查看，相关旧上下文会停止参与后续回答。
                  </p>
                  <button
                    disabled={busy}
                    onClick={() =>
                      void change(
                        confirmation === 'connection' ? '/connection' : `/sources/${confirmation}`,
                        'DELETE',
                      )
                    }
                  >
                    确认删除资料
                  </button>
                  <button onClick={() => setConfirmation(null)}>取消</button>
                </section>
              )}
              {section === 'records' && (
                <div>
                  <section className="settings-card communication-card">
                    <h2>沟通记录</h2>
                    <form className="communication-search" onSubmit={(event) => void find(event)}>
                      <label>
                        关键词或问题
                        <input
                          value={query}
                          maxLength={2000}
                          onChange={(e) => setQuery(e.target.value)}
                          placeholder="上次关于上线时间是怎么约定的？"
                        />
                      </label>
                      <button disabled={busy}>检索</button>
                      {results && (
                        <button type="button" onClick={() => setResults(null)}>
                          返回最近资料
                        </button>
                      )}
                    </form>
                    {(results || data.documents).map((doc) => (
                      <NavLink
                        className="communication-document"
                        key={doc.id}
                        to={`/communications/records/${doc.id}`}
                      >
                        <span>
                          {data.sources.find((source) => source.id === doc.source_id)?.label ||
                            '沟通记录'}{' '}
                          · {doc.day}
                        </span>
                        <small>
                          {doc.extraction_version === 0
                            ? '正在核对关联范围'
                            : doc.summary_hash
                              ? '查看整理与原文'
                              : doc.summary_error
                                ? '整理失败，将自动重试'
                                : '等待整理'}
                        </small>
                      </NavLink>
                    ))}
                    {!(results || data.documents).length && (
                      <p>{results ? '没有找到匹配资料。' : '选定会话后，记录将在后台逐步导入。'}</p>
                    )}
                  </section>
                </div>
              )}
            </>
          )}
        </>
      )}
    </div>
  );
}
