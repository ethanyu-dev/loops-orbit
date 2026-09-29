import type { Session } from '../../types';
import { OrbitMark } from '../../components/OrbitMark';
import { Composer } from './Composer';
import { MessageList } from './MessageList';
import { useChat } from './useChat';

/** 聊天页组装欢迎区、消息区和输入区，状态生命周期由 useChat 维护。 */
export function Chat({
  selected,
  setSelected,
  session,
  refreshList,
  report,
}: {
  /** 当前会话，空值显示新对话欢迎页。 */
  selected: string | null;
  /** 创建会话后同步应用导航。 */
  setSelected: (id: string) => void;
  /** 当前身份的模型展示配置。 */
  session: Session;
  /** 消息提交后刷新侧栏会话摘要。 */
  refreshList: () => Promise<void>;
  /** 向应用报告请求失败或身份失效。 */
  report: (error: unknown) => void;
}) {
  const {
    detail,
    draft,
    setDraft,
    sending,
    loading,
    pending,
    send,
    stop,
    stopping,
    failedSends,
    retrySend,
  } = useChat({
    selected,
    setSelected,
    refreshList,
    report,
  });
  const empty = !selected || (!loading && !detail.messages.length);
  return (
    <section className={`chat-view ${empty ? 'welcome-view' : ''}`}>
      {empty ? (
        <div className="welcome">
          <div className="welcome-mark">
            <OrbitMark />
          </div>
          <div className="eyebrow">YOUR PERSONAL AGENT</div>
          <h1>想法，从这里继续。</h1>
          <p>聊聊正在发生的事，让 Orbit 和你一起往前走。</p>
        </div>
      ) : (
        <MessageList detail={detail} pending={pending} loading={loading} report={report} />
      )}
      {failedSends.map((failedSend) => (
        <div className="send-retry" role="alert" key={failedSend.key}>
          上一条消息发送未确认：{failedSend.content.slice(0, 60)}
          <button disabled={sending || stopping} onClick={() => void retrySend(failedSend)}>
            重试这条消息
          </button>
        </div>
      ))}
      <Composer
        draft={draft}
        setDraft={setDraft}
        sending={sending}
        pending={!!pending.length}
        empty={empty}
        model={session.model}
        send={send}
        stop={stop}
        stopping={stopping}
      />
    </section>
  );
}
