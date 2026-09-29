import { useState, type FormEvent } from 'react';
import { api } from '../../api';
import type { Preferences } from './types';

/** 分钟数与本地时钟互转，不依赖浏览器时区。 */
function clock(minutes: number) {
  return `${String(Math.floor(minutes / 60)).padStart(2, '0')}:${String(minutes % 60).padStart(2, '0')}`;
}
/** 原生时间输入已限定格式，服务端仍校验范围。 */
function minutes(value: string) {
  const [h, m] = value.split(':').map(Number);
  return h * 60 + m;
}
/** 保存后从服务端重新加载版本；关闭回访立即取消尚未发出的回访。 */
export function PreferencesForm({
  initial,
  saved,
  report,
}: {
  initial: Preferences;
  saved: () => Promise<void>;
  report: (error: unknown) => void;
}) {
  const [form, setForm] = useState(initial);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState(false);
  async function save(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setDone(false);
    try {
      await api('/followups/preferences', { method: 'PUT', body: JSON.stringify(form) });
      setDone(true);
      await saved();
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  }
  return (
    <section className="settings-card">
      <div className="card-title">
        <h2>联系你的节奏</h2>
      </div>
      <form onSubmit={(event) => void save(event)}>
        <label className="followup-switch">
          <input
            type="checkbox"
            checked={form.enabled}
            onChange={(e) => setForm({ ...form, enabled: e.target.checked })}
          />
          允许 Orbit 主动回访
        </label>
        <p className="field-note">
          从之后的交流中记下具体的未完事项，通常两天后再判断是否适合问候一次。关闭会取消待发送的回访。
        </p>
        <label htmlFor="followup-zone">时区</label>
        <input
          id="followup-zone"
          required
          value={form.timezone}
          onChange={(e) => setForm({ ...form, timezone: e.target.value })}
          placeholder="Asia/Shanghai"
        />
        <label htmlFor="quiet-start">回访静默开始</label>
        <input
          id="quiet-start"
          type="time"
          required
          value={clock(form.quiet_start)}
          onChange={(e) => setForm({ ...form, quiet_start: minutes(e.target.value) })}
        />
        <label htmlFor="quiet-end">回访静默结束</label>
        <input
          id="quiet-end"
          type="time"
          required
          value={clock(form.quiet_end)}
          onChange={(e) => setForm({ ...form, quiet_end: minutes(e.target.value) })}
        />
        <label htmlFor="followup-interval">两次回访至少间隔（小时）</label>
        <input
          id="followup-interval"
          type="number"
          min="1"
          max="168"
          required
          value={form.min_interval_minutes / 60}
          onChange={(e) => setForm({ ...form, min_interval_minutes: Number(e.target.value) * 60 })}
        />
        <p className="field-note">
          明确设置的提醒按约定时间发送。网页通知会保留未读，飞书对话中的提醒通过飞书发送。已设置事项的时间不随时区修改移动。
        </p>
        <button className="primary-button" disabled={busy}>
          {busy ? '保存中…' : '保存偏好'}
        </button>
        {done && <p role="status">偏好已保存。</p>}
      </form>
    </section>
  );
}
