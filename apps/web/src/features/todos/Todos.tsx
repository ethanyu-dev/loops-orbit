import { useCallback, useEffect, useState } from 'react';
import { Link, useNavigate, useParams } from 'react-router-dom';
import { ArrowLeft, Bell, ChevronRight, Plus, RefreshCw, Search, Settings2 } from 'lucide-react';
import { api } from '../../api';
import { Spinner } from '../../components/Feedback';
import { PreferencesForm } from '../followups/PreferencesForm';
import type { Notifications, Preferences, Notice } from '../followups/types';
import { TodoEditor } from './TodoEditor';
import { TodoOverview } from './TodoOverview';
import { TodoHistory } from './TodoHistory';
import { Schedules } from './Schedules';
import { STATUS, type Binding, type Detail, type Listing } from './types';
import './todos.css';

// 每页与服务端一致；输入短暂停顿后再搜索，避免每个按键都重载列表。
const PAGE_SIZE = 50;
const SEARCH_DELAY = 250;
const VIEWS = { active: '待处理', scheduled: '有安排', ended: '已结束', all: '全部' };
/** 事项为主视图，通知和偏好按需展开；刷新不卸载当前详情，避免丢失编辑草稿。 */
export function Todos({
  report,
  notifications,
  refreshNotifications,
  openConversation,
}: {
  report: (error: unknown) => void;
  notifications: Notifications;
  refreshNotifications: () => Promise<void>;
  openConversation: (id: string) => void;
}) {
  const { todoId } = useParams();
  const navigate = useNavigate();
  const [list, setList] = useState<Listing | null>(null);
  const [detail, setDetail] = useState<Detail | null>(null);
  const [preferences, setPreferences] = useState<Preferences | null>(null);
  const [bindings, setBindings] = useState<Binding[]>([]);
  const [view, setView] = useState('active');
  const [search, setSearch] = useState('');
  const [query, setQuery] = useState('');
  const [offset, setOffset] = useState(0);
  const [revision, setRevision] = useState(0);
  const [loading, setLoading] = useState(true);
  const [creating, setCreating] = useState(false);
  const [busy, setBusy] = useState(false);
  const [panel, setPanel] = useState<'notifications' | 'settings' | null>(null);
  const [notice, setNotice] = useState('');
  useEffect(() => {
    const timer = setTimeout(() => {
      setQuery(search);
      setOffset(0);
    }, SEARCH_DELAY);
    return () => clearTimeout(timer);
  }, [search]);
  const refresh = useCallback(
    async (message?: string) => {
      if (message) setNotice(message);
      setRevision((v) => v + 1);
      await refreshNotifications();
    },
    [refreshNotifications],
  );
  useEffect(() => {
    let current = true;
    setLoading(true);
    void Promise.all([
      api<Listing>(`/todos?view=${view}&q=${encodeURIComponent(query)}&offset=${offset}`),
      api<Preferences>('/todos/preferences'),
      api<Binding[]>('/todos/identities'),
      todoId ? api<Detail>(`/todos/${todoId}`) : Promise.resolve(null),
    ])
      .then(([items, prefs, ids, item]) => {
        if (current) {
          setList(items);
          setPreferences(prefs);
          setBindings(ids);
          setDetail(item);
        }
      })
      .catch((error) => {
        if (current) report(error);
      })
      .finally(() => {
        if (current) setLoading(false);
      });
    return () => {
      current = false;
    };
  }, [todoId, query, offset, view, revision, report]);
  async function saved(id: string, message = '待办已保存') {
    setCreating(false);
    navigate(`/todos/${id}`);
    await refresh(message);
  }
  async function binding(item: Binding) {
    setBusy(true);
    try {
      await api('/todos/identities', {
        method: 'PUT',
        body: JSON.stringify({ owner: item.owner, version: item.version, enabled: !item.enabled }),
      });
      await refresh('本人绑定已更新');
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  }
  async function open(item: Notice) {
    try {
      await api(`/notifications/${item.id}/read`, { method: 'POST' });
      await refreshNotifications();
      openConversation(item.conversation_id);
    } catch (error) {
      report(error);
    }
  }
  const current = detail?.item.id === todoId ? detail : null;
  return (
    <div className="settings-page todo-page">
      <header className="todo-header">
        <div>
          <h1>待办事项</h1>
          <p>记下下一步，按你的节奏推进。</p>
        </div>
        <div className="todo-actions">
          <button
            className="todo-icon-button"
            title="刷新"
            aria-label="刷新待办"
            disabled={loading || busy}
            onClick={() => void refresh().catch(report)}
          >
            <RefreshCw size={16} className={loading ? 'todo-spinning' : ''} />
          </button>
          <button
            className={`secondary-button ${panel === 'notifications' ? 'is-selected' : ''}`}
            aria-expanded={panel === 'notifications'}
            aria-controls="todo-notifications"
            onClick={() => setPanel(panel === 'notifications' ? null : 'notifications')}
          >
            <Bell size={15} />
            通知
            {notifications.unread > 0 && <span className="todo-count">{notifications.unread}</span>}
          </button>
          <button
            className={`secondary-button ${panel === 'settings' ? 'is-selected' : ''}`}
            aria-expanded={panel === 'settings'}
            aria-controls="todo-settings"
            onClick={() => setPanel(panel === 'settings' ? null : 'settings')}
          >
            <Settings2 size={15} />
            偏好
          </button>
          <button
            className="primary-button"
            onClick={() => {
              navigate('/todos');
              setCreating(true);
              setNotice('');
            }}
          >
            <Plus size={16} />
            新建待办
          </button>
        </div>
      </header>
      {notice && (
        <div className="todo-feedback" role="status">
          <span>{notice}</span>
          <button className="todo-text-button" onClick={() => setNotice('')}>
            收起
          </button>
        </div>
      )}
      {panel === 'notifications' && (
        <section className="settings-card todo-panel" id="todo-notifications" aria-label="最近通知">
          <div className="todo-section-heading">
            <h2>最近通知</h2>
            <span className="todo-meta">{notifications.unread} 条未读</span>
          </div>
          {!notifications.items.length && (
            <p className="todo-empty-small">提醒和报告送达后会显示在这里。</p>
          )}
          <div className="todo-notification-list">
            {notifications.items.map((item) => (
              <article className="todo-notification" key={item.id}>
                <div className="todo-row-heading">
                  <small>
                    {new Date(item.created_at).toLocaleString()} · {item.read_at ? '已读' : '未读'}
                  </small>
                  <button className="todo-text-button" onClick={() => void open(item)}>
                    接着聊 <ChevronRight size={14} />
                  </button>
                </div>
                <details>
                  <summary className="todo-notice-preview">{item.content}</summary>
                  <p className="todo-result">{item.content}</p>
                </details>
              </article>
            ))}
          </div>
        </section>
      )}
      {panel === 'settings' && preferences && (
        <section className="settings-card todo-panel" id="todo-settings" aria-label="偏好与身份">
          <div className="todo-section-heading">
            <h2>偏好与身份</h2>
            <span className="todo-meta">网页与本人飞书共享</span>
          </div>
          <div className="todo-settings-grid">
            <PreferencesForm
              key={preferences.version}
              initial={preferences}
              saved={() => refresh('偏好已保存')}
              report={report}
            />
            <section className="todo-binding">
              <h3>本人飞书</h3>
              <p className="field-note">
                绑定后共享待办与通知。解除绑定会暂停飞书投递，待办继续保留。
              </p>
              {!bindings.length && (
                <div className="todo-empty-small">
                  <p>尚未绑定本人账号</p>
                  <Link className="todo-text-button" to="/communications">
                    前往连接飞书 <ChevronRight size={14} />
                  </Link>
                </div>
              )}
              {bindings.map((item, index) => (
                <div className="todo-binding-row" key={item.owner}>
                  <div>
                    <strong>飞书账号{bindings.length > 1 ? ` ${index + 1}` : ''}</strong>
                    <small>{item.enabled ? '共享已开启' : '已解绑'}</small>
                    <details>
                      <summary>账号标识</summary>
                      <small>{item.owner}</small>
                    </details>
                  </div>
                  <button
                    className="secondary-button"
                    disabled={busy}
                    onClick={() => void binding(item)}
                  >
                    {item.enabled ? '解除绑定' : '重新验证'}
                  </button>
                </div>
              ))}
            </section>
          </div>
        </section>
      )}
      {todoId ? (
        <>
          <Link className="todo-back" to="/todos" onClick={() => setNotice('')}>
            <ArrowLeft size={15} />
            全部待办
          </Link>
          {!current &&
            (loading ? (
              <p role="status">
                <Spinner />
                正在读取…
              </p>
            ) : (
              <p role="alert">无法读取此待办，请刷新重试。</p>
            ))}
          {current && preferences && (
            <div key={current.item.id} className="todo-detail" aria-busy={loading}>
              <div className="todo-detail-grid">
                <TodoOverview item={current.item} saved={saved} report={report} />
                <Schedules
                  item={current.item}
                  schedules={current.schedules}
                  timezone={preferences.timezone}
                  saved={refresh}
                  report={report}
                />
              </div>
              <TodoHistory detail={current} saved={refresh} report={report} />
            </div>
          )}
        </>
      ) : (
        <>
          {creating && (
            <TodoEditor saved={saved} report={report} cancel={() => setCreating(false)} />
          )}
          <div className="todo-list-toolbar">
            <div className="todo-segments" aria-label="筛选待办">
              {Object.entries(VIEWS).map(([value, label]) => (
                <button
                  key={value}
                  aria-pressed={view === value}
                  onClick={() => {
                    setView(value);
                    setOffset(0);
                  }}
                >
                  {label}
                </button>
              ))}
            </div>
            <label className="todo-search">
              <Search size={16} />
              <input
                aria-label="搜索待办"
                type="search"
                maxLength={500}
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                placeholder="搜索名称或目标"
              />
            </label>
          </div>
          <section className="todo-list" aria-label="待办列表" aria-busy={loading}>
            <div className="todo-list-caption">
              <span>{list ? `${list.total} 个事项` : '正在读取…'}</span>
              {loading && (
                <span role="status">
                  <Spinner />
                  更新中
                </span>
              )}
              <span>本人空间 · {bindings.some((item) => item.enabled) ? '双端共享' : '网页'}</span>
            </div>
            {!loading && list && !list.items.length && (
              <div className="todo-empty">
                <strong>
                  {query
                    ? '没有找到匹配的事项'
                    : view === 'active'
                      ? '暂时没有待处理事项'
                      : '这里还没有事项'}
                </strong>
                <p>
                  {query
                    ? '试试更短的名称，或切换到“全部”。'
                    : '可以新建待办，也可以在聊天中说“帮我记一下”。'}
                </p>
                {query ? (
                  <button className="secondary-button" onClick={() => setSearch('')}>
                    清空搜索
                  </button>
                ) : (
                  <button className="secondary-button" onClick={() => setCreating(true)}>
                    <Plus size={15} />
                    新建待办
                  </button>
                )}
              </div>
            )}
            {list?.items.map((item) => (
              <Link
                className="todo-list-row"
                to={`/todos/${item.id}`}
                key={item.id}
                onClick={() => {
                  setCreating(false);
                  setNotice('');
                }}
              >
                <span className={`todo-state-dot is-${item.status}`} />
                <div className="todo-list-content">
                  <strong>{item.title}</strong>
                  <span>
                    {item.waiting_on && item.status === 'waiting_external'
                      ? `等待：${item.waiting_on}`
                      : item.next_action || item.objective || '添加下一步，让事情更容易推进'}
                  </span>
                </div>
                <div className="todo-list-meta">
                  <span className={`todo-status is-${item.status}`}>{STATUS[item.status]}</span>
                  {item.due_at && (
                    <time dateTime={item.due_at}>
                      {new Date(item.due_at).toLocaleDateString('zh-CN', {
                        month: 'short',
                        day: 'numeric',
                      })}{' '}
                      截止
                    </time>
                  )}
                </div>
                <ChevronRight size={15} className="todo-row-chevron" />
              </Link>
            ))}
            {list && list.total > PAGE_SIZE && (
              <div className="todo-pagination">
                <button
                  className="secondary-button"
                  disabled={offset === 0 || loading}
                  onClick={() => setOffset(Math.max(0, offset - PAGE_SIZE))}
                >
                  上一页
                </button>
                <span>
                  第 {Math.floor(offset / PAGE_SIZE) + 1} / {Math.ceil(list.total / PAGE_SIZE)} 页
                </span>
                <button
                  className="secondary-button"
                  disabled={!list.has_more || loading}
                  onClick={() => setOffset(offset + PAGE_SIZE)}
                >
                  下一页
                </button>
              </div>
            )}
          </section>
        </>
      )}
    </div>
  );
}
