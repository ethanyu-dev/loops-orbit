import { useCallback, useEffect, useState, type FormEvent } from 'react';
import { api } from '../../api';
import { SourcePicker } from './SourcePicker';
import { DocumentView } from './DocumentView';
import type { Snapshot, Source, Document } from './types';

/** 飞书资料的独立管理入口；授权、选择来源和删除分别是明确的用户动作。 */
export function Communications({ report }: { report: (e: unknown) => void }) {
  const [data, setData] = useState<Snapshot | null>(null);
  const [selected, setSelected] = useState<string | null>(() => {
    const id = new URLSearchParams(location.search).get('communication');
    return id && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(id)
      ? id
      : null;
  });
  const [query, setQuery] = useState('');
  const [results, setResults] = useState<Document[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirmation, setConfirmation] = useState<string | null>(null);
  const [outcome] = useState(() => new URLSearchParams(location.search).get('feishu'));
  const load = useCallback(async () => setData(await api<Snapshot>('/communications/status')), []);
  useEffect(() => {
    if (outcome) history.replaceState(null, '', location.pathname + location.hash);
    void load().catch(report);
    const timer = setInterval(() => void load().catch(report), 10000);
    return () => clearInterval(timer);
  }, [load, outcome, report]);
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
      setSelected(null);
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
            ? `最近同步 ${new Date(source.last_synced_at).toLocaleString()}`
            : '等待首次同步';
  }
  return (
    <div className="settings-page communication-page">
      <h1>飞书沟通资料</h1>
      <p>把重要沟通整理成有出处的背景资料，让 Orbit 能接上你和他人的讨论。</p>
      {outcome === 'failed' && (
        <p role="alert">授权未完成。请确认账号在白名单内，应用权限和回调地址已配置，再重新连接。</p>
      )}
      {!data ? (
        <p>读取中…</p>
      ) : !data.enabled ? (
        <section className="settings-card">
          <h2>尚未启用沟通采集</h2>
          <p>需要在服务端配置用户授权和本地凭证加密密钥。机器人聊天入口可继续独立使用。</p>
        </section>
      ) : (
        <>
          <section className="settings-card communication-card">
            <h2>{data.connection ? `已连接 · ${data.connection.name}` : '连接我的飞书账号'}</h2>
            <p>
              只读你选中的会话。资料可供此管理员工作空间及同一飞书账号与 Orbit
              的对话使用，不向其他访客开放。文字会交给已配置的模型整理；图片、文件和语音不自动下载。
            </p>
            {data.connection?.status === 'reauthorize' && (
              <p role="alert">授权需要更新，采集已停止。</p>
            )}
            <button className="primary" disabled={busy} onClick={() => void connect()}>
              {data.connection ? '重新授权' : '连接飞书'}
            </button>
            {data.connection && (
              <button disabled={busy} onClick={() => setConfirmation('connection')}>
                断开并遗忘全部资料
              </button>
            )}
          </section>
          {data.connection && (
            <>
              <SourcePicker report={report} onAdded={load} />
              <section className="settings-card communication-card">
                <h2>已选择的会话 · {data.sources.length}/20</h2>
                {!data.sources.length && <p>还没有选择会话，当前不会采集聊天记录。</p>}
                {data.sources.map((source) => (
                  <article className="communication-evidence" key={source.id}>
                    <strong>{source.label}</strong>
                    <p>{status(source)}</p>
                    <div className="communication-row">
                      <button
                        disabled={busy}
                        onClick={() =>
                          void change(`/sources/${source.id}`, 'PUT', {
                            version: source.version,
                            enabled: !source.enabled,
                          })
                        }
                      >
                        {source.enabled ? '暂停' : '恢复'}
                      </button>
                      <button
                        disabled={busy || !source.enabled}
                        onClick={() => void change(`/sources/${source.id}/sync`, 'POST')}
                      >
                        立即同步
                      </button>
                      <button disabled={busy} onClick={() => setConfirmation(source.id)}>
                        遗忘此会话
                      </button>
                    </div>
                  </article>
                ))}
              </section>
              {confirmation && (
                <section className="settings-card communication-card" role="alert">
                  <h2>遗忘这些资料？</h2>
                  <p>
                    会删除
                    {confirmation === 'connection'
                      ? '所有导入资料和本地授权凭证'
                      : '此会话的原文、摘要和检索索引'}
                    ，并停止依赖这些资料的提醒。已发送的聊天记录仍可查看，相关旧上下文会停止参与后续回答。
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
                    确认遗忘
                  </button>
                  <button onClick={() => setConfirmation(null)}>取消</button>
                </section>
              )}
              <section className="settings-card communication-card">
                <h2>查找沟通记录</h2>
                <form onSubmit={(event) => void find(event)}>
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
                  <button
                    className="communication-document"
                    key={doc.id}
                    onClick={() => setSelected(doc.id)}
                  >
                    <span>
                      {data.sources.find((source) => source.id === doc.source_id)?.label ||
                        '沟通记录'}{' '}
                      · {doc.day}
                    </span>
                    <small>
                      {doc.summary_hash
                        ? '查看整理与原文'
                        : doc.summary_error
                          ? '整理失败，将自动重试'
                          : '等待整理'}
                    </small>
                  </button>
                ))}
                {!(results || data.documents).length && (
                  <p>{results ? '没有找到匹配资料。' : '选定会话后，记录将在后台逐步导入。'}</p>
                )}
              </section>
              {selected && (
                <DocumentView
                  key={selected}
                  id={selected}
                  report={report}
                  onClose={() => setSelected(null)}
                />
              )}
            </>
          )}
        </>
      )}
    </div>
  );
}
