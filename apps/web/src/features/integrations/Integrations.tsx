import { useEffect, useState } from 'react';
import { Link } from 'react-router-dom';
import { api } from '../../api';
import { apiUrl } from '../../config';
import { Spinner } from '../../components/Feedback';

/** 只展示连接身份和权限，浏览器不会获取 OAuth 令牌。 */
type Status = {
  configured: boolean;
  connection: null | {
    user_name: string;
    workspace_name: string;
    workspace_slug: string;
    scopes: string[];
    status: string;
  };
};

/** 管理员在明确选择权限后进入供应商授权页，聊天使用绑定账号执行工具。 */
export function Integrations({ report }: { report: (error: unknown) => void }) {
  const [status, setStatus] = useState<Status | null>(null);
  const [failed, setFailed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [write, setWrite] = useState(false);
  const [notice, setNotice] = useState(() => {
    const outcome = new URLSearchParams(window.location.search).get('linear');
    if (outcome === 'connected') return 'Linear 账号已连接。可以在对话中查询分配给你的 issues。';
    if (outcome === 'account_changed')
      return '授权账号或工作空间与已有连接不符。切换账号前请先断开连接。';
    if (outcome === 'failed') return '授权未完成，请检查应用配置后重新连接。';
    return '';
  });
  useEffect(() => {
    let active = true;
    api<Status>('/linear/status')
      .then((value) => {
        if (active) {
          setStatus(value);
          setWrite(value.connection?.scopes.includes('write') ?? false);
        }
      })
      .catch((error) => {
        if (active) {
          setFailed(true);
          report(error);
        }
      });
    return () => {
      active = false;
    };
  }, [report]);
  /** 仅跳转服务端生成的供应商授权地址，不把令牌写入浏览器存储。 */
  async function connect() {
    setBusy(true);
    try {
      const result = await api<{ url: string }>('/linear/oauth/start', {
        method: 'POST',
        body: JSON.stringify({ write }),
      });
      window.location.assign(result.url);
    } catch (error) {
      report(error);
      setBusy(false);
    }
  }
  /** 断开立即禁用本地工具；平台撤销失败会明确提示进一步处理。 */
  async function disconnect() {
    setBusy(true);
    try {
      const result = await api<{ provider_revoked: boolean }>('/linear/connection', {
        method: 'DELETE',
      });
      setStatus(await api<Status>('/linear/status'));
      setNotice(
        result.provider_revoked
          ? '已断开 Linear 并撤销授权。已发出的更新可能仍会完成。'
          : '本地连接已断开。未确认平台撤销，请到 Linear 的授权设置检查；已发出的更新可能仍会完成。',
      );
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="settings-page">
      <h1>外部连接</h1>
      <p className="page-description">连接工作账号，在对话中查询资料或按你的指令更新事项。</p>
      {notice && (
        <p className="page-description" role="status">
          {notice}
        </p>
      )}
      {failed ? (
        <p role="status">连接状态加载失败，请刷新页面重试。</p>
      ) : !status ? (
        <Spinner />
      ) : (
        <section className="settings-card">
          <h2>Linear</h2>
          {status.connection ? (
            <>
              <p>
                {status.connection.user_name} · {status.connection.workspace_name}（
                {status.connection.workspace_slug}）
              </p>
              <p>
                {status.connection.status === 'active' ? '已连接' : '需要重新授权'} ·{' '}
                {status.connection.scopes.includes('write') ? '允许查询和更新 issues' : '只读查询'}
              </p>
            </>
          ) : (
            <p>连接后可以询问“分配给我的 issues 有哪些”。</p>
          )}
          {!status.configured ? (
            <p>服务端尚未配置 Linear 应用。请按项目 README 配置后再连接。</p>
          ) : (
            <>
              <label>
                <input
                  type="checkbox"
                  checked={write}
                  onChange={(event) => setWrite(event.target.checked)}
                  disabled={busy}
                />{' '}
                允许按我的明确指令更新 issues
              </label>
              <div className="connection-actions">
                <button className="primary-button" disabled={busy} onClick={() => void connect()}>
                  {busy ? '处理中…' : status.connection ? '重新授权' : '连接 Linear'}
                </button>
                {status.connection && (
                  <button
                    className="secondary-button"
                    disabled={busy}
                    onClick={() => void disconnect()}
                  >
                    断开连接
                  </button>
                )}
              </div>
            </>
          )}
          <p className="page-description">
            连接仅供网页管理员使用，访客和飞书聊天不会继承该账号。修改权限以 Linear
            实际授予的范围为准。
          </p>
          <details>
            <summary>应用配置说明</summary>
            <p>
              OAuth 回调地址：<code>{apiUrl('/linear/oauth/callback')}</code>
            </p>
            <p>切换到其他账号或工作空间前先断开现有连接。</p>
          </details>
        </section>
      )}
      <p>
        <Link to="/chat">返回对话空间</Link>
      </p>
    </div>
  );
}
