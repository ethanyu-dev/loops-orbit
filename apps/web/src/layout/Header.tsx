import { ChevronRight, Menu, ShieldCheck } from 'lucide-react';
import type { Session } from '../types';
import type { Page } from './navigation';

/** 顶栏显示当前功能与身份信息，移动端菜单由应用控制。 */
export function Header({
  page,
  session,
  onOpenSidebar,
}: {
  /** 当前功能页，决定面包屑文字。 */
  page: Page;
  /** 当前身份与模型展示信息。 */
  session: Session;
  /** 请求应用展开移动端导航。 */
  onOpenSidebar: () => void;
}) {
  return (
    <header className="topbar">
      <div>
        <button className="icon-button mobile-menu" aria-label="打开侧栏" onClick={onOpenSidebar}>
          <Menu size={20} />
        </button>
        <span className="breadcrumb">工作空间</span>
        <ChevronRight size={13} />
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
        <span className="private-badge">
          <ShieldCheck size={14} />
          {session.identity.admin ? '仅你可见的工作空间' : '临时访问'}
        </span>
        <span className="model-pill">
          <span className="status-dot" />
          {session.model}
        </span>
      </div>
    </header>
  );
}
