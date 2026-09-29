import { useCallback, useEffect, useState, type FormEvent } from 'react';
import { Check, Copy, Link2, Plus, ShieldCheck } from 'lucide-react';
import { api } from '../../api';
import type { AccessLink } from '../../types';
import { Spinner } from '../../components/Feedback';
import { LinkList } from './LinkList';

// 与服务端保持一致，临时链接最长有效三十天。
const MAX_LINK_SECONDS = 30 * 24 * 3600;

/** 访问授权只在创建时显示原始链接；列表不能恢复敏感 token。 */
export function Links({ report }: { report: (e: unknown) => void }) {
  const [links, setLinks] = useState<AccessLink[]>([]);
  const [label, setLabel] = useState('');
  const [duration, setDuration] = useState(24);
  const [unit, setUnit] = useState(3600);
  const [busy, setBusy] = useState(false);
  const [created, setCreated] = useState('');
  const [copied, setCopied] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const load = useCallback(async () => {
    setLinks(await api<AccessLink[]>('/admin/links'));
    setLoaded(true);
  }, []);
  useEffect(() => {
    void load().catch(report);
  }, [load, report]);
  /** 原始链接只在创建成功时保留在页面内存，刷新列表不会重新获取密钥。 */
  async function create(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setCreated('');
    setCopied(false);
    try {
      const result = await api<{ url: string }>('/admin/links', {
        method: 'POST',
        body: JSON.stringify({ label, expires_in_seconds: duration * unit }),
      });
      setCreated(result.url);
      setLabel('');
      await load();
    } catch (e) {
      report(e);
    } finally {
      setBusy(false);
    }
  }
  /** 撤销成功后重新加载列表，以服务端状态为准。 */
  async function revoke(link: AccessLink) {
    if (!window.confirm(`撤销「${link.label}」？使用此链接登录的访客将立即失去访问权限。`)) return;
    try {
      await api(`/admin/links/${link.id}`, { method: 'DELETE' });
      await load();
    } catch (e) {
      report(e);
    }
  }
  return (
    <div className="settings-page">
      <div className="page-eyebrow">ACCESS MANAGEMENT</div>
      <h1>让好的对话，发生在一起。</h1>
      <p className="page-description">创建一个临时入口，让受邀的人也能与你的 Agent 对话。</p>
      <div className="settings-grid">
        <section className="settings-card">
          <div className="card-title">
            <Link2 size={19} />
            <h2>创建访问链接</h2>
            <span className="tag">仅聊天权限</span>
          </div>
          <form onSubmit={create}>
            <label htmlFor="link-label">链接名称</label>
            <input
              id="link-label"
              placeholder="例如：分享给朋友"
              value={label}
              onChange={(e) => setLabel(e.target.value)}
              required
              maxLength={80}
            />
            <label htmlFor="link-duration">有效期</label>
            <div className="duration-input">
              <input
                id="link-duration"
                type="number"
                min={1}
                max={Math.floor(MAX_LINK_SECONDS / unit)}
                value={duration}
                onChange={(e) => setDuration(Number(e.target.value))}
                required
              />
              <select
                aria-label="有效期单位"
                value={unit}
                onChange={(e) => setUnit(Number(e.target.value))}
              >
                <option value={60}>分钟</option>
                <option value={3600}>小时</option>
                <option value={86400}>天</option>
              </select>
            </div>
            <p className="field-note">从创建时开始计算，最长 30 天。</p>
            <button className="primary-button" disabled={busy}>
              {busy ? (
                <Spinner />
              ) : (
                <>
                  <Plus size={17} />
                  生成临时链接
                </>
              )}
            </button>
          </form>
          {created && (
            <div className="created-link">
              <div>
                <Check size={15} />
                链接已生成，请现在复制保存
              </div>
              <input
                aria-label="新访问链接"
                value={created}
                readOnly
                onFocus={(e) => e.target.select()}
              />
              <button
                onClick={async () => {
                  try {
                    await navigator.clipboard.writeText(created);
                    setCopied(true);
                  } catch (e) {
                    report(e);
                  }
                }}
              >
                {copied ? <Check size={14} /> : <Copy size={14} />}
                {copied ? '已复制' : '复制链接'}
              </button>
            </div>
          )}
        </section>
        <aside className="access-explainer">
          <div className="explainer-icon">
            <ShieldCheck size={23} />
          </div>
          <h3>分享入口，保留边界。</h3>
          <p>访客有独立的对话空间，无法查看你的历史或管理设置。管理员可以查看访客会话。</p>
          <div>
            <Check size={14} />
            到期自动失效
          </div>
          <div>
            <Check size={14} />
            随时撤销所有关联会话
          </div>
          <div>
            <Check size={14} />
            密钥仅保存在服务端
          </div>
          <p className="muted-note">同一链接的使用者共享访客空间。不同的人，建议生成不同链接。</p>
        </aside>
      </div>
      <LinkList links={links} loaded={loaded} onRevoke={revoke} />
    </div>
  );
}
