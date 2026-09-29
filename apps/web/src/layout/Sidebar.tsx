import { useState } from 'react';
import {
  Bell,
  Brain,
  Activity,
  Link2,
  LogOut,
  MessageSquare,
  Search,
  SquarePen,
} from 'lucide-react';
import type { Conversation, Session } from '../types';
import { OrbitMark } from '../components/OrbitMark';
import { formatDate } from '../lib/format';
import type { Page } from './navigation';

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
  /** 退出由服务端 Cookie 维护的会话。 */
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
  onLogout,
}: SidebarProps) {
  const [query, setQuery] = useState('');
  const [searching, setSearching] = useState(false);
  return (
    <aside className={`sidebar ${open ? 'open' : ''}`}>
      <button className="brand" onClick={onNewChat}>
        <OrbitMark small />
        <span>
          orbit<span className="brand-period">.</span>
        </span>
      </button>
      <button className="new-chat" onClick={onNewChat}>
        <SquarePen size={17} />
        新建对话<kbd>⌘ K</kbd>
      </button>
      <nav className="main-nav" aria-label="主导航">
        <button className={page === 'chat' ? 'active' : ''} onClick={() => onNavigate('chat')}>
          <MessageSquare size={17} />
          对话空间
        </button>
        <button className={page === 'memory' ? 'active' : ''} onClick={() => onNavigate('memory')}>
          <Brain size={17} />
          个人记忆
        </button>
        <button
          className={page === 'followups' ? 'active' : ''}
          onClick={() => onNavigate('followups')}
        >
          <Bell size={17} />
          提醒与跟进
          {unread > 0 && (
            <span className="tag" aria-label={`${unread} 条未读`}>
              {unread}
            </span>
          )}
        </button>
        {session.identity.admin && (
          <>
            <button
              className={page === 'communications' ? 'active' : ''}
              onClick={() => onNavigate('communications')}
            >
              <MessageSquare size={17} />
              飞书沟通资料
            </button>
            <button
              className={page === 'links' ? 'active' : ''}
              onClick={() => onNavigate('links')}
            >
              <Link2 size={17} />
              访问链接
            </button>
            <button
              className={page === 'status' ? 'active' : ''}
              onClick={() => onNavigate('status')}
            >
              <Activity size={17} />
              运行状态
            </button>
          </>
        )}
      </nav>
      <div className="history-label">
        <span>最近对话</span>
        <button
          className="icon-button"
          aria-label="搜索对话"
          onClick={() => setSearching(!searching)}
        >
          <Search size={14} />
        </button>
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
            <button
              key={c.id}
              className={page === 'chat' && selected === c.id ? 'selected' : ''}
              onClick={() => onSelect(c.id)}
            >
              <span>{c.title}</span>
              {c.channel === 'feishu' && <small>飞书</small>}
            </button>
          ))}
        {!conversations.length && <p className="history-empty">新的想法，从这里开始。</p>}
      </div>
      <div className="sidebar-bottom">
        <div className="workspace-tag">
          <span className="status-dot" />
          个人工作空间<span>V 0.1</span>
        </div>
        <div className="profile">
          <div className="avatar">{session.identity.admin ? 'O' : 'G'}</div>
          <div>
            <strong>{session.identity.admin ? '我的 Orbit' : '访客空间'}</strong>
            <small>
              {session.identity.admin
                ? '管理员'
                : `有效至 ${formatDate(session.identity.expires_at)}`}
            </small>
          </div>
          <button
            className="icon-button"
            title="退出登录"
            aria-label="退出登录"
            onClick={() => void onLogout()}
          >
            <LogOut size={16} />
          </button>
        </div>
      </div>
    </aside>
  );
}
