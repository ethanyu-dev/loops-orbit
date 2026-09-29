import { useEffect, useRef, type FormEvent } from 'react';
import { ArrowRight, ArrowUp, ChevronDown, Globe2, Sparkles, Terminal, Zap } from 'lucide-react';
import { Spinner } from '../../components/Feedback';

// 输入框随草稿增高，但保留消息区域的可见空间。
const MAX_TEXTAREA_HEIGHT = 180;
// 欢迎页提示只填充草稿，由用户确认后发送。
const SUGGESTIONS = [
  { icon: Sparkles, title: '把想法变成计划', text: '我有一个想法，帮我一起梳理成可以执行的计划。' },
  {
    icon: Terminal,
    title: '一起解决个问题',
    text: '帮我分析一个技术问题。先问我需要提供哪些信息。',
  },
  { icon: Globe2, title: '探索新的视角', text: '给我一个有意思的思考问题，帮我从不同角度探索它。' },
];

/** 输入与建议组件只负责交互，消息持久化和幂等重试交给聊天逻辑。 */
export function Composer({
  draft,
  setDraft,
  sending,
  pending,
  stop,
  stopping,
  empty,
  model,
  send,
}: {
  /** 由聊天状态维护的草稿，切换会话时同步清空。 */
  draft: string;
  /** 编辑与建议入口共用的草稿更新回调。 */
  setDraft: (value: string) => void;
  /** 消息提交中，避免重复点击。 */
  sending: boolean;
  /** 有未结束任务时提供停止入口，仍允许补充消息。 */
  pending: boolean;
  /** 取消点击时的任务。 */
  stop: () => Promise<void>;
  /** 防止停止与发送在客户端交错提交。 */
  stopping: boolean;
  /** 空会话显示输入建议。 */
  empty: boolean;
  /** 当前模型的展示名称。 */
  model: string;
  /** 发送流程由聊天逻辑执行，组件只处理输入事件。 */
  send: () => Promise<void>;
}) {
  const textarea = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    if (textarea.current) {
      textarea.current.style.height = 'auto';
      textarea.current.style.height = `${Math.min(textarea.current.scrollHeight, MAX_TEXTAREA_HEIGHT)}px`;
    }
  }, [draft]);
  /** 按钮提交和键盘提交共享流程，完成后恢复输入焦点。 */
  async function submit(event?: FormEvent) {
    event?.preventDefault();
    await send();
    textarea.current?.focus();
  }
  return (
    <div className="composer-area">
      <form className="composer" onSubmit={submit}>
        <textarea
          ref={textarea}
          aria-label="发送消息"
          placeholder={pending ? '可以继续补充，也可以改主意…' : '向 Orbit 提问，或者一起想点什么…'}
          rows={2}
          maxLength={24000}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              void submit();
            }
          }}
        />
        <div className="composer-bottom">
          <span className="composer-model">
            <Zap size={14} />
            {model}
            <ChevronDown size={13} />
          </span>
          <span className="composer-hint">Shift + Enter 换行</span>
          {pending && (
            <button
              type="button"
              className="stop-button"
              disabled={stopping || sending}
              onClick={() => void stop()}
            >
              {stopping ? '正在停止' : '停止回复'}
            </button>
          )}
          <button
            className="send-button"
            aria-label="发送"
            disabled={sending || stopping || !draft.trim()}
          >
            {sending ? <Spinner /> : <ArrowUp size={19} />}
          </button>
        </div>
      </form>
      {empty && (
        <div className="suggestions">
          {SUGGESTIONS.map((item) => (
            <button
              key={item.title}
              onClick={() => {
                setDraft(item.text);
                textarea.current?.focus();
              }}
            >
              <item.icon size={16} />
              <span>{item.title}</span>
              <ArrowRight size={14} />
            </button>
          ))}
        </div>
      )}
      <p className="composer-disclaimer">一个专注于你的 Agent。重要信息，请保持自己的判断。</p>
    </div>
  );
}
