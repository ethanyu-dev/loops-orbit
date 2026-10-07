import { LogOut, Menu } from 'lucide-react';
import { ThemeToggle } from '../features/theme/ThemeToggle';
import type { Session } from '../types';
import { formatDate } from '../lib/format';
import type { Page } from './navigation';

/** 顶栏显示当前功能，移动端菜单由应用控制。 */
export function Header({
  page,
  session,
  onLogout,
  onOpenSidebar,
}: {
  /** 当前功能页，决定标题文字。 */
  page: Page;
  /** 当前身份用于展示角色与访客有效期。 */
  session: Session;
  /** 退出由服务端 Cookie 维护的会话。 */
  onLogout: () => Promise<void>;
  /** 请求应用展开移动端导航。 */
  onOpenSidebar: () => void;
}) {
  return (
    <header className="topbar">
      <div>
        <button className="icon-button mobile-menu" aria-label="打开侧栏" onClick={onOpenSidebar}>
          <Menu size={20} />
        </button>
        <strong>
          {page === 'communications'
            ? '飞书沟通资料'
            : page === 'followups'
              ? '提醒与跟进'
              : page === 'memory'
                ? '个人记忆'
                : page === 'chat'
                  ? '对话'
                  : page === 'links'
                    ? '访问链接'
                    : '运行状态'}
        </strong>
      </div>
      <div className="topbar-right">
        <ThemeToggle />
        <div className="profile">
          <div className="avatar" aria-hidden="true">
            {session.identity.admin ? 'O' : 'G'}
          </div>
          <div className="profile-identity">
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
    </header>
  );
}
