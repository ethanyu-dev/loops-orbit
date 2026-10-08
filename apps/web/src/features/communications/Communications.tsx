import { useEffect, useState } from 'react';
import { api } from '../../api';
import { NavLink, Navigate, useLocation, useNavigate, useSearchParams } from 'react-router-dom';
import { CONVERSATION_ID } from '../../layout/navigation';
import { HistoryProgress } from './HistoryProgress';
import { HistoryImport } from './HistoryImport';
import './history.css';
import { SourceManager } from './SourceManager';
import { ConnectionCard } from './ConnectionCard';
import { Spinner } from '../../components/Feedback';
import { DocumentView } from './DocumentView';
import { DocumentLibrary } from './DocumentLibrary';
import { useCommunicationSnapshot } from './useCommunicationSnapshot';

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
      await load();
    } catch (e) {
      report(e);
    } finally {
      setBusy(false);
    }
  }
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
        onClose={() => void navigate(`/communications/records${location.search}`)}
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
                <NavLink to="/communications/sources">
                  订阅管理 · {data.sources.filter((source) => source.subscribed !== false).length}
                </NavLink>
              </nav>
              {section === 'sources' && <SourceManager data={data} reload={load} report={report} />}
              {section === 'sync' && (
                <div className="sync-workspace">
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
                  <HistoryProgress
                    revision={historyRevision}
                    documents={data.progress}
                    onRefresh={load}
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
              {section === 'records' && <DocumentLibrary sources={data.sources} />}
            </>
          )}
        </>
      )}
    </div>
  );
}
