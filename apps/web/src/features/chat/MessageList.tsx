import { Fragment, useEffect, useRef, useState } from 'react';
import { ArrowDown, Check, Copy } from 'lucide-react';
import { useMessageScroll } from './useMessageScroll';
import { Answer } from './Answer';
import { runError } from './runError';
import type { Detail, Run } from '../../types';
import { OrbitMark } from '../../components/OrbitMark';
import { Spinner } from '../../components/Feedback';

// 复制成功提示短暂显示，避免长期遮蔽复制入口。
const COPY_FEEDBACK_MS = 2000;

/** 渲染消息、失败提示与排队状态；Markdown 保持禁用原始 HTML。 */
export function MessageList({
  detail,
  pending,
  loading,
  report,
}: {
  /** 已加载的消息和对应任务状态。 */
  detail: Detail;
  /** 当前会话尚未结束的任务。 */
  pending: Run[];
  /** 首次详情请求期间显示加载提示。 */
  loading: boolean;
  /** 复制失败时由应用显示错误。 */
  report: (error: unknown) => void;
}) {
  const copyTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(copyTimer.current), []);
  const [copied, setCopied] = useState<number | null>(null);
  const { viewport, content, atBottom, onScroll, scrollToLatest } = useMessageScroll();
  /** 复制状态局限于消息列表，不影响会话或任务状态。 */
  async function copy(id: number, content: string) {
    try {
      await navigator.clipboard.writeText(content);
      setCopied(id);
      clearTimeout(copyTimer.current);
      copyTimer.current = setTimeout(() => setCopied(null), COPY_FEEDBACK_MS);
    } catch {
      report(new Error('clipboard'));
    }
  }
  return (
    <div className="message-region">
      <div className="message-scroll" ref={viewport} onScroll={onScroll}>
        <div className="messages" ref={content}>
          {loading && (
            <div className="loading-row">
              <Spinner />
              正在读取对话
            </div>
          )}
          {detail.messages.map((message) => {
            const run = detail.runs.find((r) => r.id === message.run_id);
            return (
              <Fragment key={message.id}>
                <div className={`message ${message.role}`} key={message.id}>
                  {message.role === 'assistant' && (
                    <div className="assistant-avatar">
                      <OrbitMark small />
                    </div>
                  )}
                  <div className="message-content">
                    {message.role === 'assistant' ? (
                      <>
                        <div className="assistant-label">
                          {message.kind === 'followup' ? 'Orbit · 主动消息' : 'Orbit'}
                        </div>
                        <Answer content={message.content} />
                        <button
                          title={copied === message.id ? '已复制' : '复制回复'}
                          className="copy-message"
                          aria-label="复制回复"
                          onClick={() => void copy(message.id, message.content)}
                        >
                          {copied === message.id ? <Check size={14} /> : <Copy size={14} />}
                          <span>{copied === message.id ? '已复制' : '复制'}</span>
                        </button>
                      </>
                    ) : (
                      <>
                        {message.content}
                        {run?.status === 'cancelled' && (
                          <small className="run-note">已停止回复</small>
                        )}
                        {run?.status === 'superseded' && (
                          <small className="run-note">已与后续消息一起处理</small>
                        )}
                        {run?.status === 'failed' && (
                          <div className="run-error">
                            {run.phase === 'saved_reply_failed'
                              ? '待办操作已保存，后续回复生成失败。请查看下方提交记录。'
                              : runError(run.error)}
                            <small>{run?.error}</small>
                          </div>
                        )}
                      </>
                    )}
                  </div>
                </div>
                {message.role === 'user' && run?.status === 'running' && run.partial_content && (
                  <div className="message assistant" aria-busy="true">
                    <div className="assistant-avatar">
                      <OrbitMark small />
                    </div>
                    <div className="message-content">
                      <div className="assistant-label">Orbit · 正在回复</div>
                      <Answer content={run.partial_content} />
                    </div>
                  </div>
                )}
              </Fragment>
            );
          })}
          {/* 已有流式正文时由消息自身提示进度，避免重复显示两组正在回复。 */}
          {pending.length > 0 && !pending.some((run) => run.partial_content) && (
            <div className="thinking" role="status">
              <OrbitMark small />
              <span className="thinking-dots">
                <i />
                <i />
                <i />
              </span>
              <span>
                {pending.some((r) => r.phase === 'context')
                  ? '正在衔接前面的对话'
                  : pending.some((r) => r.status === 'running')
                    ? '正在思考'
                    : '正在接收你的补充'}
              </span>
            </div>
          )}
        </div>
      </div>
      {!atBottom && (
        <button className="jump-to-latest" onClick={scrollToLatest} aria-label="回到最新消息">
          <ArrowDown size={16} /> 最新消息
        </button>
      )}
    </div>
  );
}
