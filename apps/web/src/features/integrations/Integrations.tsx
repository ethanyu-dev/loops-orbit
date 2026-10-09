import { useState } from 'react';
import { Link } from 'react-router-dom';
import {
  ArrowLeft,
  ArrowRight,
  CheckCircle2,
  CircleAlert,
  Link2,
  RefreshCw,
  Unplug,
  X,
} from 'lucide-react';
import { Spinner } from '../../components/Feedback';
import { ConnectionHelp } from './ConnectionHelp';
import { DisconnectDialog } from './DisconnectDialog';
import { useLinearConnection } from './useLinearConnection';
import './integrations.css';

/** 连接状态、主要操作和按需帮助分层展示，权限最终仍由服务端及 Linear 检查。 */
export function Integrations({ report }: { report: (error: unknown) => void }) {
  const { status, loading, action, feedback, refresh, run, clearFeedback } =
    useLinearConnection(report);
  const [confirmDisconnect, setConfirmDisconnect] = useState(false);
  const connected = status?.configured && status.connection?.status === 'active';
  const needsAttention = status?.configured && status.connection && !connected;
  const busy = action !== null || loading;
  const stateLabel = !status?.configured
    ? '待配置'
    : connected
      ? '已连接'
      : needsAttention
        ? '需检查'
        : '待连接';

  /** 失败保留现有账号状态，并关闭确认框使用户能立即看到具体错误。 */
  async function disconnect() {
    await run('disconnect');
    setConfirmDisconnect(false);
  }

  return (
    <div className="settings-page integrations-page">
      <header className="integration-page-header">
        <Link to="/chat" className="integration-back">
          <ArrowLeft size={15} aria-hidden="true" />
          返回对话
        </Link>
        <h1>外部连接</h1>
        <p className="page-description">连接你的工作账号，在对话中查询任务、更新进展。</p>
      </header>

      <div className="integration-feedback-slot">
        {feedback && (
          <div
            className={`integration-feedback is-${feedback.tone}`}
            role={feedback.tone === 'error' ? 'alert' : 'status'}
          >
            {feedback.tone === 'error' ? (
              <CircleAlert size={17} aria-hidden="true" />
            ) : (
              <CheckCircle2 size={17} aria-hidden="true" />
            )}
            <span>{feedback.text}</span>
            <button aria-label="关闭提示" onClick={clearFeedback}>
              <X size={15} aria-hidden="true" />
            </button>
          </div>
        )}
      </div>

      <section className="integration-card" aria-labelledby="linear-heading" aria-busy={busy}>
        <div className="integration-card-header">
          <div className="integration-provider">
            <span className="integration-provider-icon">
              <Link2 size={24} aria-hidden="true" />
            </span>
            <div>
              <h2 id="linear-heading">Linear</h2>
              <p>项目与任务</p>
            </div>
          </div>
          {status && !loading && (
            <span
              className={`integration-badge ${connected ? 'is-connected' : needsAttention ? 'is-warning' : ''}`}
            >
              <span />
              {stateLabel}
            </span>
          )}
        </div>

        {loading ? (
          <div className="integration-placeholder" role="status">
            <Spinner />
            正在读取连接状态…
          </div>
        ) : !status ? (
          <div className="integration-placeholder">
            <p>暂时无法读取连接状态</p>
            <button className="integration-button" onClick={() => void refresh()}>
              <RefreshCw size={15} aria-hidden="true" />
              重新加载
            </button>
          </div>
        ) : (
          <>
            {status.connection ? (
              <dl className="integration-identity">
                <div>
                  <dt>连接账号</dt>
                  <dd>{status.connection.user_name}</dd>
                </div>
                <div>
                  <dt>工作空间</dt>
                  <dd>
                    {status.connection.workspace_name}
                    <span>{status.connection.workspace_slug}</span>
                  </dd>
                </div>
              </dl>
            ) : (
              <div className="integration-intro">
                <h3>
                  {status.configured ? '准备好连接你的 Linear' : '连接 Linear，从对话处理待办'}
                </h3>
                <p>
                  {status.configured
                    ? '已检测到服务端密钥，验证后即可查看账号与工作空间。'
                    : '先按下方帮助配置个人 API Key，再返回此页验证连接。'}
                </p>
              </div>
            )}
            {needsAttention && (
              <p className="integration-warning">密钥已失效。请检查服务端配置后重新验证连接。</p>
            )}
            {connected && (
              <div className="integration-example">
                <span>试着问一句</span>
                <p>“分配给我的任务有哪些？”</p>
                <small>也可以按你的明确指令更新状态、优先级或负责人。</small>
              </div>
            )}

            <div className="integration-actions">
              {connected && (
                <Link className="integration-button integration-button-primary" to="/chat">
                  前往对话
                  <ArrowRight size={16} aria-hidden="true" />
                </Link>
              )}
              {status.configured ? (
                <button
                  className={`integration-button ${connected ? '' : 'integration-button-primary'}`}
                  disabled={busy}
                  onClick={() => void run('verify')}
                >
                  {action === 'verify' ? <Spinner /> : <RefreshCw size={15} aria-hidden="true" />}
                  {action === 'verify'
                    ? '正在验证…'
                    : status.connection
                      ? '重新验证'
                      : '验证并连接'}
                </button>
              ) : (
                <button
                  className="integration-button"
                  disabled={busy}
                  onClick={() => void refresh()}
                >
                  <RefreshCw size={15} aria-hidden="true" />
                  重新检测配置
                </button>
              )}
              {status.connection && (
                <button
                  className="integration-disconnect"
                  disabled={busy}
                  onClick={() => setConfirmDisconnect(true)}
                >
                  <Unplug size={15} aria-hidden="true" />
                  断开连接
                </button>
              )}
            </div>
            <p className="integration-scope">
              仅用于你的 Orbit 网页对话，操作范围以 Linear 权限为准。
            </p>
          </>
        )}
      </section>
      <ConnectionHelp needsSetup={status?.configured === false} />
      <DisconnectDialog
        open={confirmDisconnect}
        busy={action === 'disconnect'}
        onClose={() => setConfirmDisconnect(false)}
        onConfirm={() => void disconnect()}
      />
    </div>
  );
}
