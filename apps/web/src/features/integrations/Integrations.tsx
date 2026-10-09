import { useEffect, useState } from 'react';
import { Link } from 'react-router-dom';
import { api } from '../../api';
import { Spinner } from '../../components/Feedback';

/** 只展示连接身份与状态，浏览器不会获取个人 API Key。 */
type Status = {
  configured: boolean;
  connection: null | {
    user_name: string;
    workspace_name: string;
    workspace_slug: string;
    status: string;
  };
};

/** Orbit 所有者验证服务端配置的个人密钥，聊天使用实际账号执行工具。 */
export function Integrations({ report }: { report: (error: unknown) => void }) {
  const [status, setStatus] = useState<Status | null>(null);
  const [failed, setFailed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState('');
  useEffect(() => {
    let active = true;
    api<Status>('/linear/status')
      .then((value) => {
        if (active) {
          setStatus(value);
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
  /** 验证密钥对应的身份；密钥由部署环境管理，网页不读取或提交密钥。 */
  async function connect() {
    setBusy(true);
    setNotice('');
    try {
      await api('/linear/connection', { method: 'POST' });
      setStatus(await api<Status>('/linear/status'));
      setNotice('Linear 账号已连接。可以在对话中查询或按明确指令更新 issues。');
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  }
  /** 断开只禁用 Orbit 连接，个人密钥需在 Linear 中自行撤销。 */
  async function disconnect() {
    setBusy(true);
    setNotice('');
    try {
      await api('/linear/connection', { method: 'DELETE' });
      setStatus(await api<Status>('/linear/status'));
      setNotice(
        '已断开 Orbit 中的连接。若需撤销个人 API Key，请到 Linear 操作；已发出的更新可能仍会完成。',
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
                {status.connection.status === 'active'
                  ? '已连接'
                  : '密钥失效，请检查配置后重新连接'}
              </p>
            </>
          ) : (
            <p>连接后可以询问“分配给我的 issues 有哪些”。</p>
          )}
          {!status.configured ? (
            <p>服务端尚未配置 Linear 个人 API Key。请按下方说明配置后再连接。</p>
          ) : (
            <>
              <div className="connection-actions">
                <button className="primary-button" disabled={busy} onClick={() => void connect()}>
                  {busy ? '处理中…' : status.connection ? '重新验证连接' : '验证并连接'}
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
            查询和更新范围由你的个人 API Key 权限及团队访问范围决定，无需 Linear 管理员权限。
            连接仅供 Orbit 网页所有者使用，访客和飞书聊天不会继承该账号。
          </p>
          <details>
            <summary>个人 API Key 配置说明</summary>
            <p>
              在 Linear 的 Settings → Account → Security &amp; Access 中创建个人 API Key， 选择 Read
              和 Write 权限及需要访问的团队。若没有创建权限，需工作空间管理员开启成员 API Key 功能。
            </p>
            <p>
              在服务端设置 <code>LINEAR_API_KEY</code> 后重启，再点击「验证并连接」。 可选设置{' '}
              <code>LINEAR_WORKSPACE_SLUG</code> 限定工作空间。 更换密钥后需要重新验证连接。
            </p>
          </details>
        </section>
      )}
      <p>
        <Link to="/chat">返回对话空间</Link>
      </p>
    </div>
  );
}
