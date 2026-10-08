import { useEffect, useRef, useState } from 'react';
import { NavLink } from 'react-router-dom';
import { MessageSquare, Search, Plus, X } from 'lucide-react';
import type { Conversation, Session } from '../types';
import { OrbitMark } from '../components/OrbitMark';
import { conversationPath, type Page } from './navigation';
import { WorkspaceMenu } from './WorkspaceMenu';

/** 侧栏只维护搜索展开与关键词，身份、导航和会话选择由应用统一管理。 */
interface SidebarProps {
  /** 当前会话身份，决定管理入口是否可见。 */
  session: Session;
  /** 当前功能页。 */
  page: Page;
  /** 当前选中的会话标识。 */
  selected: string | null;
  /** 当前身份可见的最近会话。 */
  conversations: Conversation[];
  /** 移动端侧栏是否展开。 */
  open: boolean;
  /** 仅统计当前身份实际收到的未读主动消息。 */
  unread: number;
  /** 返回新对话。 */
  onNewChat: () => void;
  /** 切换功能页并收起移动端侧栏。 */
  onNavigate: (page: Page) => void;
  /** 选择历史会话并清除旧错误。 */
  onSelect: (id: string) => void;
  /** 收起移动端侧栏。 */
  onClose: () => void;
  /** 退出当前登录。 */
  onLogout: () => Promise<void>;
}

/** 工作空间导航与会话索引，不直接请求接口。 */
export function Sidebar({
  session,
  page,
  selected,
  conversations,
  open,
  unread,
  onNewChat,
  onNavigate,
  onSelect,
  onClose,
  onLogout,
}: SidebarProps) {
  const panel = useRef<HTMLElement>(null);
  const [query, setQuery] = useState('');
  const [searching, setSearching] = useState(false);
  useEffect(() => {
    if (!open) return;
    const previous = document.activeElement as HTMLElement | null;
    panel.current?.querySelector<HTMLElement>('button')?.focus();
    /** 移动侧栏打开时约束 Tab 顺序，关闭后回到原来的入口。 */
    function trapFocus(event: KeyboardEvent) {
      if (event.key !== 'Tab') return;
      const controls = Array.from(
        panel.current?.querySelectorAll<HTMLElement>('button, a, input') ?? [],
      ).filter(
        (element) => !element.hasAttribute('disabled') && element.getClientRects().length > 0,
      );
      const first = controls[0];
      const last = controls[controls.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last?.focus();
      }
      if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first?.focus();
      }
    }
    document.addEventListener('keydown', trapFocus);
    return () => {
      document.removeEventListener('keydown', trapFocus);
      previous?.focus();
    };
  }, [open]);
  return (
    <aside
      ref={panel}
      id="workspace-sidebar"
      aria-label="会话侧栏"
      className={`sidebar ${open ? 'open' : ''}`}
    >
      <div className="sidebar-tools">
        <button className="brand" onClick={onNewChat} aria-label="Orbit 新对话">
          <OrbitMark small />
          <span>
            orbit<span className="brand-period">.</span>
          </span>
        </button>
        <button
          className="icon-button"
          aria-label="搜索对话"
          title="搜索对话"
          aria-expanded={searching}
          onClick={() => {
            setSearching(!searching);
            setQuery('');
          }}
        >
          <Search size={19} />
        </button>
        <button
          className="icon-button"
          aria-label="新建对话"
          title="新建对话（⌘ / Ctrl K）"
          onClick={onNewChat}
        >
          <Plus size={21} />
        </button>
        <button className="icon-button sidebar-close" aria-label="关闭侧栏" onClick={onClose}>
          <X size={20} />
        </button>
      </div>
      <div className="history-label">
        <span>最近对话</span>
        <span>{conversations.length || ''}</span>
      </div>
      {searching && (
        <input
          className="history-search"
          autoFocus
          placeholder="搜索最近对话…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          aria-label="搜索最近对话"
        />
      )}
      <div className="history-list">
        {conversations
          .filter((c) => c.title.toLowerCase().includes(query.toLowerCase()))
          .map((c) => (
            <NavLink
              to={conversationPath(c.id)}
              key={c.id}
              className={page === 'chat' && selected === c.id ? 'selected' : ''}
              onClick={() => onSelect(c.id)}
            >
              <MessageSquare size={17} aria-hidden="true" />
              <span>{c.title}</span>
              {c.channel === 'feishu' && <small>飞书</small>}
            </NavLink>
          ))}
        {!conversations.length && <p className="history-empty">新的想法，从这里开始。</p>}
        {conversations.length > 0 &&
          !conversations.some((c) => c.title.toLowerCase().includes(query.toLowerCase())) && (
            <p className="history-empty">没有找到相关对话，换个关键词试试。</p>
          )}
      </div>
      <WorkspaceMenu
        session={session}
        page={page}
        unread={unread}
        onNavigate={onNavigate}
        onLogout={onLogout}
      />
    </aside>
  );
}
