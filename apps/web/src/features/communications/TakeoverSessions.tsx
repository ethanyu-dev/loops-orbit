import { useState } from 'react';
import { api } from '../../api';

// 仅显示产品状态，不把队列内部状态当成用户的操作选项。
const MODES: Record<string, string> = {
  auto: '自动回复',
  human: '本人处理中',
  paused: '已暂停',
  uncertain: '发送待核对',
};

/** 服务端持久化的私聊控制状态，不包含消息正文或凭证。 */
export interface TakeoverSession {
  /** 来源标识，服务端将其绑定到真实私聊。 */
  source_id: string;
  /** 联系人显示名称。 */
  label: string;
  /** 当前控制模式。 */
  mode: string;
  /** 页面操作的并发版本。 */
  version: number;
  /** 上次成功回答的话题。 */
  topic: string | null;
  /** 临时本人接管的自动恢复边界。 */
  human_until_ms: number | null;
  /** 最近活动时间。 */
  updated_at: string;
  /** 已停用或移除的来源不能通过此处恢复订阅。 */
  subscribed: boolean;
}

/** 紧凑会话列表；恢复建立新边界，未知投递需先核对飞书。 */
export function TakeoverSessions({
  sessions,
  selected,
  onSelect,
  onChange,
  report,
}: {
  sessions: TakeoverSession[];
  selected: string | null;
  onSelect: (id: string | null) => void;
  onChange: () => Promise<void>;
  report: (error: unknown) => void;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const [notice, setNotice] = useState('');
  /** 只提交读取到的版本，冲突时刷新状态供本人重新选择。 */
  async function control(session: TakeoverSession, mode: string) {
    setBusy(session.source_id);
    setNotice('');
    try {
      await api(`/communications/takeover/sessions/${session.source_id}`, {
        method: 'PUT',
        body: JSON.stringify({ mode, version: session.version }),
      });
      setNotice(
        mode === 'auto'
          ? '已恢复，只处理之后的新消息。'
          : mode === 'human'
            ? '已由你接手，双方 30 分钟无人发言后恢复。'
            : '已暂停，恢复前不会自动回复。',
      );
      await onChange();
    } catch (error) {
      report(error);
      await onChange().catch(() => {});
    } finally {
      setBusy(null);
    }
  }
  return (
    <section className="settings-card">
      <div className="takeover-section-title">
        <h2>会话</h2>
        <small>手动恢复只处理新消息</small>
      </div>
      {notice && <p role="status">{notice}</p>}
      {sessions.length === 0 ? (
        <p>新的私聊问题被采集后，会显示在这里。</p>
      ) : (
        <ul className="takeover-sessions">
          {sessions.map((session) => (
            <li
              key={session.source_id}
              className={selected === session.source_id ? 'selected' : ''}
            >
              <button
                className="takeover-session-select"
                aria-pressed={selected === session.source_id}
                onClick={() => onSelect(selected === session.source_id ? null : session.source_id)}
              >
                <strong>{session.label}</strong>
                <small>
                  {session.topic || '尚无已回答话题'} ·{' '}
                  {new Date(session.updated_at).toLocaleString()}
                </small>
              </button>
              <div className="takeover-session-controls">
                <span className={`takeover-mode ${session.mode}`}>
                  {session.subscribed ? MODES[session.mode] : '来源已停用'}
                </span>
                {session.subscribed && (
                  <>
                    {session.mode === 'auto' && (
                      <button
                        disabled={busy !== null}
                        onClick={() => void control(session, 'human')}
                      >
                        我来处理
                      </button>
                    )}
                    {(session.mode === 'auto' || session.mode === 'human') && (
                      <button
                        disabled={busy !== null}
                        onClick={() => void control(session, 'paused')}
                      >
                        暂停自动回复
                      </button>
                    )}
                    {session.mode !== 'auto' && (
                      <button
                        disabled={busy !== null}
                        onClick={() => void control(session, 'auto')}
                      >
                        {session.mode === 'uncertain' ? '已核对，恢复自动' : '恢复自动'}
                      </button>
                    )}
                  </>
                )}
              </div>
              {session.mode === 'uncertain' && (
                <p className="takeover-session-hint">
                  请先在飞书核对上一条回复是否发出。恢复不会重发或回答积压消息。
                </p>
              )}
              {session.mode === 'human' && (
                <p className="takeover-session-hint">
                  双方持续 30 分钟没有发言后，新问题可恢复自动处理；也可手动恢复。
                </p>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
