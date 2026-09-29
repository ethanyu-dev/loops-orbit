import { useCallback, useEffect, useState } from 'react';
import { X } from 'lucide-react';
import { api, ApiError, errorText } from './api';
import type { Conversation, Session } from './types';
import { OrbitMark } from './components/OrbitMark';
import { Spinner } from './components/Feedback';
import { Sidebar } from './layout/Sidebar';
import { Header } from './layout/Header';
import type { Page } from './layout/navigation';
import { Login } from './features/auth/Login';
import { Chat } from './features/chat/Chat';
import { useConversationSelection } from './features/chat/useConversationSelection';
import { Links } from './features/links/Links';
import { Memory } from './features/memory/Memory';
import { Followups } from './features/followups/Followups';
import { useNotifications } from './features/followups/useNotifications';
import { Health } from './features/status/Health';
import { Communications } from './features/communications/Communications';

/** 全局身份和页面状态不持久化凭证，访客到期由 API 的每次认证决定。 */
export function App({ incomingToken }: { incomingToken: string | null }) {
  const [session, setSession] = useState<Session | null>(null);
  const [booting, setBooting] = useState(true);
  const [error, setError] = useState('');
  const [page, setPage] = useState<Page>(() =>
    new URLSearchParams(location.search).has('feishu') ||
    new URLSearchParams(location.search).has('communication')
      ? 'communications'
      : 'chat',
  );
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [selected, setSelected] = useConversationSelection();
  const [sidebar, setSidebar] = useState(false);
  const report = useCallback(
    (e: unknown) => {
      setError(errorText(e));
      if (e instanceof ApiError && e.status === 401) {
        setSession(null);
        setConversations([]);
        setSelected(null);
      }
    },
    [setSelected],
  );
  const notifications = useNotifications(session?.identity.owner, report);
  const loadSession = useCallback(async () => {
    setSession(await api<Session>('/me'));
    setError('');
  }, []);
  const loadConversations = useCallback(async () => {
    setConversations(await api<Conversation[]>('/conversations'));
  }, []);
  useEffect(() => {
    (async () => {
      try {
        if (incomingToken)
          await api('/auth/exchange', {
            method: 'POST',
            body: JSON.stringify({ token: incomingToken }),
          });
        await loadSession();
      } catch (e) {
        if (incomingToken || !(e instanceof ApiError && e.status === 401)) report(e);
      } finally {
        setBooting(false);
      }
    })();
  }, [incomingToken, loadSession, report]);
  useEffect(() => {
    if (session) void loadConversations().catch(report);
  }, [session, loadConversations, report]);
  const newChat = useCallback(() => {
    setPage('chat');
    setSelected(null);
    setSidebar(false);
    setError('');
  }, [setSelected]);
  useEffect(() => {
    const shortcut = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key === 'k') {
        event.preventDefault();
        newChat();
      }
      if (event.key === 'Escape') setSidebar(false);
    };
    window.addEventListener('keydown', shortcut);
    return () => window.removeEventListener('keydown', shortcut);
  }, [newChat]);
  /** 服务端注销成功后再清理本地身份和导航。 */
  async function logout() {
    try {
      await api('/auth/logout', { method: 'POST' });
      setSession(null);
      setSelected(null);
      setConversations([]);
      setPage('chat');
      setError('');
    } catch (e) {
      report(e);
    }
  }
  if (booting)
    return (
      <div className="boot">
        <OrbitMark />
        <Spinner />
      </div>
    );
  if (!session) return <Login error={error} onLogin={loadSession} report={report} />;
  return (
    <div className="app-shell">
      {sidebar && (
        <button className="scrim" aria-label="关闭侧栏" onClick={() => setSidebar(false)} />
      )}
      <Sidebar
        session={session}
        page={page}
        selected={selected}
        conversations={conversations}
        open={sidebar}
        unread={notifications.data.unread}
        onNewChat={newChat}
        onNavigate={(nextPage) => {
          setPage(nextPage);
          setSidebar(false);
        }}
        onSelect={(id) => {
          setSelected(id);
          setPage('chat');
          setSidebar(false);
          setError('');
        }}
        onLogout={logout}
      />
      <main className="main-panel">
        <Header page={page} session={session} onOpenSidebar={() => setSidebar(true)} />
        {error && (
          <div className="global-error" role="alert">
            {error}
            <button className="icon-button" aria-label="关闭提示" onClick={() => setError('')}>
              <X size={16} />
            </button>
          </div>
        )}
        {page === 'chat' && (
          <Chat
            selected={selected}
            setSelected={setSelected}
            session={session}
            refreshList={loadConversations}
            report={report}
          />
        )}
        {page === 'followups' && (
          <Followups
            report={report}
            notifications={notifications.data}
            refreshNotifications={notifications.refresh}
            openConversation={(id) => {
              setSelected(id);
              setPage('chat');
              void loadConversations().catch(report);
            }}
          />
        )}
        {page === 'memory' && <Memory report={report} />}
        {page === 'communications' && session.identity.admin && <Communications report={report} />}
        {page === 'links' && session.identity.admin && <Links report={report} />}
        {page === 'status' && session.identity.admin && <Health report={report} />}
      </main>
    </div>
  );
}
