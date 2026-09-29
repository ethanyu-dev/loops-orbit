import { Link2, Trash2 } from 'lucide-react';
import type { AccessLink } from '../../types';
import { Empty, Spinner } from '../../components/Feedback';
import { formatDate } from '../../lib/format';

/** 仅展示授权元数据；有效状态决定是否允许发起撤销操作。 */
export function LinkList({
  links,
  loaded,
  onRevoke,
}: {
  /** 最近创建的授权元数据。 */
  links: AccessLink[];
  /** 区分首次加载与空列表。 */
  loaded: boolean;
  /** 撤销确认与服务端写入由页面处理。 */
  onRevoke: (link: AccessLink) => Promise<void>;
}) {
  return (
    <section className="links-section">
      <div className="section-heading">
        <h2>
          已创建的链接 <span>{links.length}</span>
        </h2>
        <span>最近 100 条</span>
      </div>
      {!loaded ? (
        <Empty>
          <Spinner />
        </Empty>
      ) : !links.length ? (
        <Empty>
          <Link2 size={24} />
          <p>还没有访问链接</p>
          <span>创建第一个链接，邀请一次新的对话。</span>
        </Empty>
      ) : (
        <div className="links-table">
          <div className="table-heading">
            <span>名称</span>
            <span>状态</span>
            <span>到期时间</span>
            <span />
          </div>
          {links.map((link) => {
            const expired = new Date(link.expires_at).getTime() <= Date.now();
            const active = !expired && !link.revoked_at;
            return (
              <div className="table-row" key={link.id}>
                <div>
                  <Link2 size={16} />
                  <strong>{link.label}</strong>
                </div>
                <span className={`link-status ${active ? 'valid' : ''}`}>
                  <i />
                  {link.revoked_at ? '已撤销' : expired ? '已过期' : '有效'}
                </span>
                <time>{formatDate(link.expires_at)}</time>
                <button
                  className="icon-button"
                  aria-label={`撤销 ${link.label}`}
                  title="撤销链接"
                  disabled={!active}
                  onClick={() => void onRevoke(link)}
                >
                  <Trash2 size={16} />
                </button>
              </div>
            );
          })}
        </div>
      )}
    </section>
  );
}
