import { useState } from 'react';
import { apiUrl } from '../../config';
import { ArrowUpRight, Check, Copy, MessageSquare } from 'lucide-react';
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
      <section className={`connection-hero ${connected ? 'is-connected' : ''}`}>
        <div className="connection-copy">
          <div className="connection-status">
            <span className={connected ? 'status-dot' : 'neutral-dot'} />
            {connected ? '账号已连接' : data.connection ? '需要重新授权' : '等待连接'}
          </div>
          <h2>{data.connection ? data.connection.name : '连接飞书'}</h2>
          {!data.connection && (
            <p>手动选择要订阅的会话，在对话中查找与你相关的讨论、决定和待办。</p>
          )}
          {data.connection?.status === 'reauthorize' && (
            <p className="login-error" role="alert">
              授权已失效，采集已暂停。请重新授权后继续。
            </p>
          )}
          <div className="connection-actions">
            <button
              className={connected ? 'secondary-button' : 'primary-button'}
              disabled={busy}
              onClick={() => void connect()}
            >
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
        </div>
      </section>
      <details className="connection-details">
        <summary>数据范围与授权帮助</summary>
        <div className="connection-detail-grid">
          <div>
            <h3>你始终掌握范围</h3>
            <p>
              仅同步你手动订阅的会话，历史由你选择日期补录。单聊保留全部消息；群聊只提取你发送、明确
              @你或直接回复你的消息。资料仅供此管理员工作空间及同一飞书账号使用。文字和可访问的图片交给已配置的多模态模型整理，图片资源按需读取，不公开带授权的链接；文件和语音暂不解析。你可以暂停或移除会话。
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
