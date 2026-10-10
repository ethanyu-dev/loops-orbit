import { lazy, Suspense, useCallback, useEffect, useState } from 'react';
import { Link, Navigate, Route, Routes, useLocation } from 'react-router-dom';
import { Menu, X } from 'lucide-react';
import { api, ApiError, errorText } from './api';
import type { Conversation, Session } from './types';
import { OrbitMark } from './components/OrbitMark';
import { Spinner } from './components/Feedback';
import { PageBoundary } from './components/PageBoundary';
import { Sidebar } from './layout/Sidebar';
import { useWorkspaceNavigation } from './layout/useWorkspaceNavigation';
import { Login } from './features/auth/Login';
import { useNotifications } from './features/followups/useNotifications';

// 功能页按路由加载，首屏登录和聊天无需下载整个管理界面。
const Chat = lazy(() => import('./features/chat/Chat').then((m) => ({ default: m.Chat })));
const Integrations = lazy(() =>
  import('./features/integrations/Integrations').then((m) => ({ default: m.Integrations })),
);
const Links = lazy(() => import('./features/links/Links').then((m) => ({ default: m.Links })));
const Memory = lazy(() => import('./features/memory/Memory').then((m) => ({ default: m.Memory })));
const Knowledge = lazy(() =>
  import('./features/knowledge/Knowledge').then((m) => ({ default: m.Knowledge })),
);
const Todos = lazy(() => import('./features/todos/Todos').then((m) => ({ default: m.Todos })));
const Health = lazy(() => import('./features/status/Health').then((m) => ({ default: m.Health })));
const Communications = lazy(() =>
  import('./features/communications/Communications').then((m) => ({ default: m.Communications })),
);

/** 无效地址和无权限入口有明确反馈，避免显示空白工作空间。 */
function Unavailable({ forbidden = false }: { forbidden?: boolean }) {
  return (
    <div className="settings-page" role="status">
      <h1>{forbidden ? '此页面需要管理员权限' : '页面不存在'}</h1>
      <Link to="/chat">返回对话空间</Link>
    </div>
  );
}

/** 全局身份和页面状态不持久化凭证，访客到期由 API 的每次认证决定。 */
export function App({ incomingToken }: { incomingToken: string | null }) {
  const location = useLocation();
  const [session, setSession] = useState<Session | null>(null);
  const [booting, setBooting] = useState(true);
  const [error, setError] = useState('');
  const {
    page,
    selected,
    setSelected,
    newChat: navigateNewChat,
    invalidConversation,
  } = useWorkspaceNavigation();
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [sidebar, setSidebar] = useState(false);
  // 身份失效保留当前 URL，重新登录后可回到原页面；回调不依赖导航，避免换页重跑 token 兑换。
  const report = useCallback((e: unknown) => {
    setError(errorText(e));
    if (e instanceof ApiError && e.status === 401) {
      setSession(null);
      setConversations([]);
    }
  }, []);
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
    navigateNewChat();
    setSidebar(false);
    setError('');
  }, [navigateNewChat]);
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
  useEffect(() => {
    const desktop = window.matchMedia('(min-width: 761px)');
    const closeOnDesktop = () => {
      if (desktop.matches) setSidebar(false);
    };
    desktop.addEventListener('change', closeOnDesktop);
    return () => desktop.removeEventListener('change', closeOnDesktop);
  }, []);
  /** 服务端注销成功后再清理本地身份和导航。 */
  async function logout() {
    try {
      await api('/auth/logout', { method: 'POST' });
      setSession(null);
      setSelected(null);
      setConversations([]);
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
        onClose={() => setSidebar(false)}
        onLogout={logout}
        onNewChat={newChat}
        onNavigate={() => {
          setSidebar(false);
        }}
        onSelect={() => {
          setSidebar(false);
          setError('');
        }}
      />
      <main className="main-panel" inert={sidebar}>
        <button
          className="icon-button mobile-menu"
          aria-label="打开侧栏"
          aria-expanded={sidebar}
          aria-controls="workspace-sidebar"
          onClick={() => setSidebar(true)}
        >
          <Menu size={20} />
        </button>
        {error && (
          <div className="global-error" role="alert">
            {error}
            <button className="icon-button" aria-label="关闭提示" onClick={() => setError('')}>
              <X size={16} />
            </button>
          </div>
        )}
        <PageBoundary resetKey={location.pathname}>
          <Suspense
            fallback={
              <div className="loading-row">
                <Spinner />
                正在加载页面…
              </div>
            }
          >
            <Routes>
              <Route path="/" element={<Navigate to="/chat" replace />} />
              <Route
                path="/chat/:conversationId?"
                element={
                  invalidConversation ? (
                    <Unavailable />
                  ) : (
                    <Chat
                      selected={selected}
                      setSelected={setSelected}
                      session={session}
                      refreshList={loadConversations}
                      report={report}
                    />
                  )
                }
              />
              <Route path="/followups" element={<Navigate to="/todos" replace />} />
              <Route
                path="/todos/:todoId?"
                element={
                  session.identity.admin ? (
                    <Todos
                      report={report}
                      notifications={notifications.data}
                      refreshNotifications={notifications.refresh}
                      openConversation={(id) => {
                        setSelected(id);
                        void loadConversations().catch(report);
                      }}
                    />
                  ) : (
                    <Unavailable forbidden />
                  )
                }
              />
              <Route
                path="/integrations"
                element={
                  session.identity.admin ? (
                    <Integrations report={report} />
                  ) : (
                    <Unavailable forbidden />
                  )
                }
              />
              <Route path="/memory" element={<Memory report={report} />} />
              <Route
                path="/knowledge"
                element={
                  session.identity.admin ? <Knowledge report={report} /> : <Unavailable forbidden />
                }
              />
              <Route
                path="/communications/*"
                element={
                  session.identity.admin ? (
                    <Communications report={report} />
                  ) : (
                    <Unavailable forbidden />
                  )
                }
              />
              <Route
                path="/links"
                element={
                  session.identity.admin ? <Links report={report} /> : <Unavailable forbidden />
                }
              />
              <Route
                path="/status"
                element={
                  session.identity.admin ? <Health report={report} /> : <Unavailable forbidden />
                }
              />
              <Route path="*" element={<Unavailable />} />
            </Routes>
          </Suspense>
        </PageBoundary>
      </main>
    </div>
  );
}
