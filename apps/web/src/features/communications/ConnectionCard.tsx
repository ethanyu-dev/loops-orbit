import { useState } from 'react';
import { apiUrl } from '../../config';
import { ArrowUpRight, Check, Copy, FileText, MessageSquare, ShieldCheck } from 'lucide-react';
import { OrbitMark } from '../../components/OrbitMark';
import { Spinner } from '../../components/Feedback';
import type { Snapshot } from './types';

/** 连接状态和授权说明集中展示；不把启用配置误报为已授权或正在采集。 */
export function ConnectionCard({
  data,
  busy,
  connect,
  disconnect,
  report,
}: {
  /** 服务端当前授权及采集状态。 */
  data: Snapshot;
  /** 正在请求授权，阻止重复提交。 */
  busy: boolean;
  /** 进入飞书本人授权流程。 */
  connect: () => Promise<void>;
  /** 打开已有的断开确认流程。 */
  disconnect: () => void;
  /** 复制失败沿用应用错误提示。 */
  report: (error: unknown) => void;
}) {
  const [copied, setCopied] = useState(false);
  const connected = data.connection?.status === 'active';
  const callback = apiUrl('/communications/oauth/callback');
  /** 只复制公开回调地址，不接触授权令牌。 */
  async function copyCallback() {
    try {
      await navigator.clipboard.writeText(callback);
      setCopied(true);
    } catch (error) {
      report(error);
    }
  }
  return (
    <>
      <section className="connection-hero">
        <div className="connection-copy">
          <div className="connection-status">
            <span className={connected ? 'status-dot' : 'neutral-dot'} />
            {connected ? '账号已连接' : data.connection ? '需要重新授权' : '等待连接'}
          </div>
          <h2>
            {data.connection
              ? `你好，${data.connection.name}`
              : '让重要的沟通，\n成为下一次对话的背景。'}
          </h2>
          <p>连接飞书，自动订阅今后的沟通。Orbit 会记下讨论中的决定和待办，让每次回答都有来处。</p>
          {data.connection?.status === 'reauthorize' && (
            <p className="login-error" role="alert">
              授权已失效，采集已暂停。请重新授权后继续。
            </p>
          )}
          <div className="connection-actions">
            <button className="primary-button" disabled={busy} onClick={() => void connect()}>
              {busy ? <Spinner /> : <MessageSquare size={17} />}
              {data.connection ? '重新授权' : '连接我的飞书'}
              <ArrowUpRight size={17} />
            </button>
            {data.connection && (
              <button className="text-button" disabled={busy} onClick={disconnect}>
                断开并遗忘
              </button>
            )}
          </div>
          <span className="connection-assurance">
            <ShieldCheck size={14} />
            授权后自动同步新消息，历史范围由你选择
          </span>
        </div>
        <div className="connection-visual" aria-hidden="true">
          <div className="orbital-track" />
          <div className="orbital-track second" />
          <div className="connection-orbit">
            <OrbitMark />
          </div>
          <span className="orbit-node node-message">
            <MessageSquare size={24} />
          </span>
          <span className="orbit-node node-file">
            <FileText size={22} />
          </span>
          <span className="orbit-node node-check">
            <Check size={22} />
          </span>
          <span className="orbit-caption">每一次沟通，都有迹可循</span>
        </div>
      </section>
      <div className="connection-steps" aria-label="连接步骤">
        {[
          ['01', '授权你的账号', '由你决定 Orbit 能访问什么。'],
          ['02', '新消息自动同步', '历史消息按需选择日期整理。'],
          ['03', '接着聊下去', '约每 10 分钟同步，保留原话出处。'],
        ].map(([number, title, description], index) => (
          <div key={number} className="connection-step">
            <span>
              {(index === 0 && connected) || (index === 1 && data.sources.length > 0) ? (
                <Check size={15} />
              ) : (
                number
              )}
            </span>
            <h3>{title}</h3>
            <p>{description}</p>
          </div>
        ))}
      </div>
      <details className="connection-details">
        <summary>数据范围与授权帮助</summary>
        <div className="connection-detail-grid">
          <div>
            <h3>你始终掌握范围</h3>
            <p>
              自动收集授权范围内可发现、可读取会话的新消息，历史由你选择日期补录。资料仅供此管理员工作空间及同一飞书账号使用。文字和可访问的图片交给已配置的多模态模型整理，图片资源按需读取，不公开带授权的链接；文件和语音暂不解析。你可以暂停或移除会话。
            </p>
          </div>
          <div>
            <h3>遇到“Invalid redirect URL”？</h3>
            <p>
              在飞书应用的安全设置中添加以下 OAuth
              回调地址，保存后从本页重新连接。这与机器人事件回调是两个不同地址。
            </p>
            <div className="callback-address">
              <code>{callback}</code>
              <button
                className="icon-button"
                aria-label="复制 OAuth 回调地址"
                onClick={() => void copyCallback()}
              >
                {copied ? <Check size={16} /> : <Copy size={16} />}
              </button>
            </div>
            <span className="copy-status" role="status">
              {copied ? '已复制回调地址' : ''}
            </span>
          </div>
        </div>
      </details>
    </>
  );
}
