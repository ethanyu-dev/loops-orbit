import { useEffect, useRef, useState } from 'react';
import { NavLink } from 'react-router-dom';
import {
  Activity,
  Bell,
  Brain,
  BookOpen,
  ChevronUp,
  Link2,
  LogOut,
  MessageSquare,
  Settings2,
} from 'lucide-react';
import type { Session } from '../types';
import { formatDate } from '../lib/format';
import { ThemeToggle } from '../features/theme/ThemeToggle';
import { PAGE_PATHS, type Page } from './navigation';

// 所有功能入口收进账号菜单，管理员入口仍按当前身份过滤。
const WORKSPACE_ITEMS = [
  { page: 'chat', label: '对话空间', icon: MessageSquare, admin: false },
  { page: 'memory', label: '个人记忆', icon: Brain, admin: false },
  { page: 'knowledge', label: '通用知识库', icon: BookOpen, admin: true },
  { page: 'followups', label: '提醒与跟进', icon: Bell, admin: false },
  { page: 'communications', label: '飞书沟通资料', icon: MessageSquare, admin: true },
  { page: 'integrations', label: '外部连接', icon: Link2, admin: true },
  { page: 'links', label: '访问链接', icon: Link2, admin: true },
  { page: 'status', label: '运行状态', icon: Activity, admin: true },
] satisfies { page: Page; label: string; icon: typeof Brain; admin: boolean }[];

/** 固定在侧栏底部的身份与二级功能入口，关闭后保留触发按钮的键盘焦点。 */
export function WorkspaceMenu({
  session,
  page,
  unread,
  onNavigate,
  onLogout,
}: {
  /** 当前身份及访客有效期。 */
  session: Session;
  /** 当前功能页，用于标记选中入口。 */
  page: Page;
  /** 收起菜单时仍提示未读通知。 */
  unread: number;
  /** 功能导航后关闭移动端侧栏。 */
  onNavigate: (page: Page) => void;
  /** 交由应用撤销登录会话。 */
  onLogout: () => Promise<void>;
}) {
  const [expanded, setExpanded] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!expanded) return;
    /** 点击外部或按 Escape 收起，不拦截普通链接及表单交互。 */
    function dismiss(event: PointerEvent | KeyboardEvent) {
      if (event instanceof KeyboardEvent) {
        if (event.key !== 'Escape') return;
        event.stopPropagation();
        trigger.current?.focus();
      } else if (root.current?.contains(event.target as Node)) return;
      setExpanded(false);
    }
    document.addEventListener('pointerdown', dismiss);
    document.addEventListener('keydown', dismiss);
    return () => {
      document.removeEventListener('pointerdown', dismiss);
      document.removeEventListener('keydown', dismiss);
    };
  }, [expanded]);
  return (
    <div
      className="workspace-account"
      ref={root}
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget)) setExpanded(false);
      }}
    >
      <button
        ref={trigger}
        className="account-trigger"
        aria-label="账号与工作空间菜单"
        aria-expanded={expanded}
        aria-controls="workspace-menu"
        onClick={() => setExpanded(!expanded)}
      >
        <span className="avatar" aria-hidden="true">
          {session.identity.admin ? 'O' : 'G'}
        </span>
        <span className="profile-identity">
          <strong>{session.identity.admin ? '我的 Orbit' : '访客空间'}</strong>
          <small>
            {session.identity.admin
              ? '管理员 · 工作空间'
              : `有效至 ${formatDate(session.identity.expires_at)}`}
          </small>
        </span>
        {unread > 0 && <span className="unread-dot" aria-label={`${unread} 条未读通知`} />}
        <ChevronUp size={16} className={expanded ? 'expanded' : ''} />
      </button>
      {expanded && (
        <div className="workspace-menu" id="workspace-menu">
          <div className="workspace-menu-heading">
            <Settings2 size={14} />
            工作空间
          </div>
          <nav aria-label="工作空间功能">
            {WORKSPACE_ITEMS.filter((item) => !item.admin || session.identity.admin).map((item) => (
              <NavLink
                key={item.page}
                to={PAGE_PATHS[item.page]}
                className={page === item.page ? 'active' : ''}
                onClick={() => {
                  setExpanded(false);
                  onNavigate(item.page);
                }}
              >
                <item.icon size={17} />
                <span>{item.label}</span>
                {item.page === 'followups' && unread > 0 && <span className="tag">{unread}</span>}
              </NavLink>
            ))}
          </nav>
          <div className="workspace-menu-footer">
            <ThemeToggle />
            <button
              className="logout-button"
              onClick={() => {
                setExpanded(false);
                void onLogout();
              }}
            >
              <LogOut size={15} />
              退出登录
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
