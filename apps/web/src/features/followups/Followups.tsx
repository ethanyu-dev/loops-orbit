import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react';
import { ArrowDown, Bell, Plus, RefreshCw } from 'lucide-react';
import { api } from '../../api';
import { Spinner } from '../../components/Feedback';
import { PreferencesForm } from './PreferencesForm';
import type { Followup, Notice, Notifications, Snapshot } from './types';

// 展示真实状态；发送成功不等于用户已经完成事项。
const LABELS: Record<string, string> = {
  scheduled: '等待到期',
  checking: '正在判断',
  queued: '等待投递',
  sent: '已发送',
  completed: '已完成',
  cancelled: '已取消',
  expired: '已过期',
  failed: '未能发送',
};
const ACTIVE = ['scheduled', 'checking', 'queued', 'sent'];
/** 用事项时区展示绝对时间，避免浏览器所在地造成歧义。 */
function display(time: string, zone: string) {
  return new Date(time).toLocaleString('zh-CN', { timeZone: zone, hour12: false });
}
/** 本地分钟表单始终由服务端按显式展示的偏好时区解释。 */
function localTime(time: string, zone: string) {
  const parts = new Intl.DateTimeFormat('en-CA', {
    timeZone: zone,
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hourCycle: 'h23',
  }).formatToParts(new Date(time));
  const get = (name: string) => parts.find((p) => p.type === name)?.value;
  return `${get('year')}-${get('month')}-${get('day')}T${get('hour')}:${get('minute')}`;
}
/** 统一管理明确提醒、轻量回访与已发送通知。 */
export function Followups({
  report,
  notifications,
  refreshNotifications,
  openConversation,
}: {
  report: (error: unknown) => void;
  notifications: Notifications;
  refreshNotifications: () => Promise<void>;
  openConversation: (id: string) => void;
}) {
  const [data, setData] = useState<Snapshot | null>(null);
  const [topic, setTopic] = useState('');
  const [due, setDue] = useState('');
  const [editing, setEditing] = useState<Followup | null>(null);
  const [key, setKey] = useState(() => crypto.randomUUID());
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState('');
  const [refreshing, setRefreshing] = useState(false);
  const [refreshStatus, setRefreshStatus] = useState('');
  const refreshInFlight = useRef(false);
  const notificationsPanel = useRef<HTMLElement>(null);
  const load = useCallback(async () => {
    setData(await api<Snapshot>('/followups'));
  }, []);
  useEffect(() => {
    void load().catch(report);
  }, [load, report]);
  /** 等两个请求均结束后恢复入口；刷新失败不误报成功，也不清空编辑中的草稿。 */
  async function refresh() {
    if (refreshInFlight.current) return;
    refreshInFlight.current = true;
    setRefreshing(true);
    setRefreshStatus('');
    try {
      const results = await Promise.allSettled([load(), refreshNotifications()]);
      const failure = results.find((result) => result.status === 'rejected');
      if (failure?.status === 'rejected') {
        setRefreshStatus('刷新未完成，请重试');
        report(failure.reason);
      } else {
        setRefreshStatus('已更新');
      }
    } finally {
      refreshInFlight.current = false;
      setRefreshing(false);
    }
  }
  /** 查看通知只移动阅读位置，实际打开某条通知后才标记已读。 */
  function showNotifications() {
    const panel = notificationsPanel.current;
    panel?.focus({ preventScroll: true });
    panel?.scrollIntoView({ block: 'start', behavior: 'auto' });
  }
  /** 一次创建在网络重试时保持相同幂等键；改期使用编辑开始时看到的版本。 */
  async function save(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    try {
      const result = await api<{ delivery_may_have_started?: boolean }>(
        `/followups${editing ? `/${editing.id}` : ''}`,
        {
          method: editing ? 'PUT' : 'POST',
          body: JSON.stringify(
            editing
              ? { version: editing.version, status: 'scheduled', topic, due_at: due }
              : { idempotency_key: key, kind: 'reminder', topic, due_at: due },
          ),
        },
      );
      setTopic('');
      setDue('');
      setEditing(null);
      setKey(crypto.randomUUID());
      setNotice(
        result.delivery_may_have_started
          ? '已改期；原消息已开始投递，仍可能收到。'
          : '已保存，到时会在对应会话中提醒你。',
      );
      await load();
    } catch (error) {
      report(error);
      await load().catch(report);
    } finally {
      setBusy(false);
    }
  }
  /** 已投递内容不撤回，取消只停止尚未发送的版本及其重试。 */
  async function change(item: Followup, status: string) {
    setBusy(true);
    try {
      const result = await api<{ delivery_may_have_started: boolean }>(`/followups/${item.id}`, {
        method: 'PUT',
        body: JSON.stringify({ version: item.version, status }),
      });
      setNotice(
        result.delivery_may_have_started
          ? '状态已更新；原消息已开始投递，仍可能收到。'
          : '状态已更新。',
      );
      await load();
    } catch (error) {
      report(error);
      await load().catch(report);
    } finally {
      setBusy(false);
    }
  }
  /** 只确认用户打开的这一条通知，不把事项自动标为完成。 */
  async function open(item: Notice) {
    try {
      await api(`/notifications/${item.id}/read`, { method: 'POST' });
      await refreshNotifications();
      openConversation(item.conversation_id);
    } catch (error) {
      report(error);
    }
  }
  if (!data)
    return (
      <div className="settings-page">
        <Spinner />
      </div>
    );
  return (
    <div className="settings-page followup-page">
      <div className="page-eyebrow">FOLLOW THROUGH</div>
      <h1>惦记着，也给你留空间。</h1>
      <p className="page-description">
        明确的提醒按时送达。允许主动回访后，Orbit 会在合适的时候接着聊一件未完的事。
      </p>
      <div className="followup-toolbar">
        <button className="notification-shortcut" onClick={showNotifications}>
          <Bell size={16} />
          <span>
            {notifications.unread ? `${notifications.unread} 条未读通知` : '暂无未读通知'}
          </span>
          <span className="notification-shortcut-hint">查看通知</span>
          <ArrowDown size={14} />
        </button>
        <div className="followup-refresh">
          <span className="refresh-status" role="status">
            {refreshStatus}
          </span>
          <button
            className="refresh-button"
            disabled={busy || refreshing}
            onClick={() => void refresh()}
            aria-label="刷新提醒与通知"
          >
            {refreshing ? <Spinner /> : <RefreshCw size={15} />}
            <span>{refreshing ? '刷新中…' : '刷新'}</span>
          </button>
        </div>
      </div>
      {notice && (
        <p className="memory-notice" role="status">
          {notice}
        </p>
      )}
      <div className="settings-grid">
        <section className="settings-card">
          <div className="card-title">
            <Bell size={19} />
            <h2>{editing ? '调整时间' : '记下一次提醒'}</h2>
          </div>
          <form onSubmit={(event) => void save(event)}>
            <label htmlFor="followup-topic">到时提醒你什么</label>
            <input
              id="followup-topic"
              required
              maxLength={500}
              placeholder="例如：把整理好的方案发给同事"
              value={topic}
              onChange={(e) => {
                setTopic(e.target.value);
                setKey(crypto.randomUUID());
              }}
            />
            <label htmlFor="followup-due">提醒时间（{data.preferences.timezone}）</label>
            <input
              id="followup-due"
              type="datetime-local"
              required
              value={due}
              onChange={(e) => {
                setDue(e.target.value);
                setKey(crypto.randomUUID());
              }}
            />
            <p className="field-note">
              单次提醒；服务恢复时会补发一天内错过的提醒，超过截止时间则过期。也可以在对话里说“明天下午三点提醒我”。
            </p>
            <div className="memory-actions">
              <button className="primary-button" disabled={busy}>
                <Plus size={16} />
                {busy ? '保存中…' : '保存提醒'}
              </button>
              {editing && (
                <button
                  type="button"
                  className="secondary-button"
                  onClick={() => {
                    setEditing(null);
                    setTopic('');
                    setDue('');
                  }}
                >
                  退出编辑
                </button>
              )}
            </div>
          </form>
        </section>
        <PreferencesForm
          key={data.preferences.version}
          initial={data.preferences}
          saved={async () => {
            setNotice('偏好已保存。');
            await load();
          }}
          report={report}
        />
      </div>
      <section className="settings-card followup-section">
        <div className="card-title">
          <h2>已经记下的事</h2>
        </div>
        {!data.items.length && (
          <p className="field-note">还没有提醒。可以在上方添加，也可以直接在对话里告诉 Orbit。</p>
        )}
        {data.items.map((item) => (
          <article className="memory-entry" key={item.id}>
            <div className="memory-entry-title">
              <strong>{item.topic}</strong>
              <span className="tag">
                {item.kind === 'reminder' ? '提醒' : '回访'} · {LABELS[item.status] || item.status}
              </span>
            </div>
            <p>
              {display(item.due_at, item.timezone)} · {item.timezone}
            </p>
            <small>
              补发截止：{display(item.expires_at, item.timezone)}
              {item.error === 'memory_changed'
                ? ' · 相关记忆已修改或遗忘'
                : item.status === 'failed'
                  ? ' · 可调整时间后重试'
                  : ''}
            </small>
            <div className="memory-actions">
              <button
                className="secondary-button"
                onClick={() => openConversation(item.conversation_id)}
              >
                打开对话
              </button>
              {item.error !== 'memory_changed' && (
                <button
                  className="secondary-button"
                  disabled={busy}
                  onClick={() => {
                    setEditing(item);
                    setTopic(item.topic);
                    setDue(localTime(item.due_at, data.preferences.timezone));
                    document.getElementById('followup-topic')?.focus();
                  }}
                >
                  改期
                </button>
              )}
              {ACTIVE.includes(item.status) && (
                <>
                  <button
                    className="secondary-button"
                    disabled={busy}
                    onClick={() => void change(item, 'completed')}
                  >
                    已完成
                  </button>
                  <button
                    className="secondary-button"
                    disabled={busy}
                    onClick={() => void change(item, 'cancelled')}
                  >
                    取消
                  </button>
                </>
              )}
            </div>
          </article>
        ))}
      </section>
      <section
        ref={notificationsPanel}
        tabIndex={-1}
        aria-labelledby="recent-notifications-title"
        className="settings-card followup-section notifications-panel"
      >
        <div className="card-title">
          <h2 id="recent-notifications-title">最近通知</h2>
        </div>
        {!notifications.items.length && (
          <p className="field-note">
            提醒送达后会出现在这里。网页关闭期间的通知会保留，下次打开时查看。
          </p>
        )}
        {notifications.items.map((item) => (
          <article className="memory-entry" key={item.id}>
            <div className="memory-entry-title">
              <small>{new Date(item.created_at).toLocaleString()}</small>
              {!item.read_at && <span className="tag">未读</span>}
            </div>
            <p>{item.content}</p>
            <button className="secondary-button" onClick={() => void open(item)}>
              接着聊
            </button>
          </article>
        ))}
      </section>
    </div>
  );
}
