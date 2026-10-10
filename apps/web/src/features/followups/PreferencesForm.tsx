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
      await api('/todos/preferences', { method: 'PUT', body: JSON.stringify(form) });
      setDone(true);
      await saved();
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  }
  return (
    <section className="todo-preferences">
      <div className="todo-section-heading">
        <h3>联系你的节奏</h3>
      </div>
      <form onSubmit={(event) => void save(event)}>
        <label className="followup-switch">
          <input
            type="checkbox"
            checked={form.enabled}
            onChange={(e) => setForm({ ...form, enabled: e.target.checked })}
          />
          从交流中发现待办候选
        </label>
        <p className="field-note">
          从之后的交流中提出待确认事项，接受并设置安排后才开始回访。单独安排的回访无需开启此开关；从开启切为关闭时，会一并停止尚未发出的回访和回访安排。
        </p>
        <div className="todo-form-grid">
          <label>
            时区
            <input
              required
              value={form.timezone}
              onChange={(e) => setForm({ ...form, timezone: e.target.value })}
              placeholder="Asia/Shanghai"
            />
          </label>
          <label>
            回访间隔（小时）
            <input
              type="number"
              min="1"
              max="168"
              required
              value={form.min_interval_minutes / 60}
              onChange={(e) =>
                setForm({ ...form, min_interval_minutes: Number(e.target.value) * 60 })
              }
            />
          </label>
          <label>
            静默开始
            <input
              type="time"
              required
              value={clock(form.quiet_start)}
              onChange={(e) => setForm({ ...form, quiet_start: minutes(e.target.value) })}
            />
          </label>
          <label>
            静默结束
            <input
              type="time"
              required
              value={clock(form.quiet_end)}
              onChange={(e) => setForm({ ...form, quiet_end: minutes(e.target.value) })}
            />
          </label>
        </div>
        <p className="field-note">静默起止相同表示不静默；修改时区不会移动已有安排。</p>
        <button className="primary-button" disabled={busy}>
          {busy ? '保存中…' : '保存偏好'}
        </button>
        {done && <p role="status">偏好已保存。</p>}
      </form>
    </section>
  );
}
