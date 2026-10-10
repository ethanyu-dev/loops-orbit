import { useState, type FormEvent } from 'react';
import { Plus } from 'lucide-react';
import { api } from '../../api';
import {
  KINDS,
  RULES,
  SCHEDULE_STATUS,
  localTime,
  type Schedule,
  type ScheduleInput,
  type Todo,
} from './types';

/** 每个安排单独编辑，投递位置沿用当前安排，用户修改才切换。 */
export function Schedules({
  item,
  schedules,
  timezone,
  saved,
  report,
}: {
  item: Todo;
  schedules: Schedule[];
  timezone: string;
  saved: (message?: string) => Promise<void>;
  report: (error: unknown) => void;
}) {
  const [editing, setEditing] = useState<Schedule | null | undefined>(undefined);
  const [busy, setBusy] = useState(false);

  async function stop(schedule: Schedule, status: string) {
    setBusy(true);
    try {
      const result = await api<{ delivery_may_have_started: boolean }>(
        `/todos/${item.id}/schedules`,
        {
          method: 'POST',
          body: JSON.stringify({
            todo_version: item.version,
            id: schedule.id,
            version: schedule.version,
            status,
            idempotency_key: crypto.randomUUID(),
          }),
        },
      );
      await saved(
        result.delivery_may_have_started
          ? '已停止后续处理；已经开始投递的消息仍可能送达。'
          : '安排已更新。',
      );
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  }
  const closed = ['completed', 'cancelled'].includes(item.status);
  return (
    <section className="settings-card">
      <div className="todo-section-heading">
        <h2>执行安排</h2>
        <button
          className="secondary-button"
          disabled={closed || busy || editing !== undefined}
          onClick={() => setEditing(null)}
        >
          <Plus size={14} />
          添加安排
        </button>
      </div>
      {!schedules.length && (
        <p className="todo-empty-small">需要到时提醒或主动跟进？添加一个安排。</p>
      )}
      {schedules.map((s) => (
        <article className="todo-schedule" key={s.id}>
          <div className="todo-row-heading">
            <strong>
              {KINDS[s.kind]} <span className="todo-meta">· {RULES[s.recurrence]}</span>
            </strong>
            <span className="todo-status">{SCHEDULE_STATUS[s.status]}</span>
          </div>
          <p className="todo-meta">
            {s.channel === 'feishu' ? '飞书' : '网页'} · {s.timezone}
          </p>
          <p>
            {s.status === 'enabled' ? '下次' : '计划时间'}：
            {new Date(s.next_run_at).toLocaleString('zh-CN', { timeZone: s.timezone })}
          </p>
          {s.instruction && (
            <details className="todo-disclosure">
              <summary>处理说明</summary>
              <p className="todo-result">{s.instruction}</p>
            </details>
          )}
          <div className="todo-form-actions">
            <button
              className="secondary-button"
              disabled={closed || busy}
              onClick={() => setEditing(s)}
            >
              {s.status === 'enabled' ? '编辑' : '恢复安排'}
            </button>
            {s.status === 'enabled' && (
              <button
                className="secondary-button"
                disabled={busy}
                onClick={() => void stop(s, 'paused')}
              >
                暂停
              </button>
            )}
            {s.status !== 'ended' && (
              <button
                className="secondary-button"
                disabled={busy}
                onClick={() => void stop(s, 'ended')}
              >
                结束安排
              </button>
            )}
          </div>
        </article>
      ))}
      {editing !== undefined && (
        <ScheduleEditor
          key={editing?.id ?? 'new'}
          item={item}
          initial={editing}
          timezone={timezone}
          done={async () => {
            setEditing(undefined);
            await saved('安排已保存');
          }}
          cancel={() => setEditing(undefined)}
          report={report}
        />
      )}
    </section>
  );
}
/** 恢复必须选择新的未来时间；同一提交的重试使用固定幂等键。 */
function ScheduleEditor({
  item,
  initial,
  timezone,
  done,
  cancel,
  report,
}: {
  item: Todo;
  initial: Schedule | null;
  timezone: string;
  done: () => Promise<void>;
  cancel: () => void;
  report: (error: unknown) => void;
}) {
  const [todoVersion] = useState(item.version);
  const [form, setForm] = useState<ScheduleInput>(() =>
    initial
      ? {
          kind: initial.kind,
          next_run_at: localTime(initial.next_run_at, initial.timezone),
          timezone: initial.timezone,
          recurrence: initial.recurrence,
          ends_at: initial.ends_at,
          missed_policy: initial.missed_policy,
          grace_minutes: initial.grace_minutes,
          instruction: initial.instruction,
          channel: initial.channel,
        }
      : {
          kind: 'reminder',
          next_run_at: '',
          timezone,
          recurrence: 'once',
          ends_at: null,
          missed_policy: 'latest',
          grace_minutes: 1440,
          instruction: '',
          channel: 'web',
        },
  );
  const [key, setKey] = useState(() => crypto.randomUUID());
  const [busy, setBusy] = useState(false);
  function change<K extends keyof ScheduleInput>(field: K, value: ScheduleInput[K]) {
    setForm((old) => ({ ...old, [field]: value }));
    setKey(crypto.randomUUID());
  }
  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    try {
      await api(`/todos/${item.id}/schedules`, {
        method: 'POST',
        body: JSON.stringify({
          todo_version: todoVersion,
          id: initial?.id,
          version: initial?.version,
          status: 'enabled',
          schedule: form,
          idempotency_key: key,
        }),
      });
      await done();
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  }
  return (
    <form
      className="todo-schedule-form"
      onSubmit={(e) => void submit(e)}
      onInvalid={(event) => {
        // 高级字段校验失败时展开所在区域，让浏览器能聚焦并说明错误。
        const section = (event.target as HTMLElement).closest('details');
        if (section) section.open = true;
      }}
    >
      <h3>{initial ? '编辑安排' : '添加安排'}</h3>
      <div className="todo-form-grid">
        <label>
          处理方式
          <select value={form.kind} onChange={(e) => change('kind', e.target.value)}>
            {Object.entries(KINDS).map(([v, l]) => (
              <option key={v} value={v}>
                {l}
              </option>
            ))}
          </select>
        </label>
        <label>
          重复
          <select value={form.recurrence} onChange={(e) => change('recurrence', e.target.value)}>
            {Object.entries(RULES).map(([v, l]) => (
              <option key={v} value={v}>
                {l}
              </option>
            ))}
          </select>
        </label>
        <label className="todo-full">
          下次时间 <span className="todo-optional">{form.timezone}</span>
          <input
            type="datetime-local"
            required
            value={form.next_run_at}
            onChange={(e) => change('next_run_at', e.target.value)}
          />
        </label>
        <label>
          通知位置
          <select value={form.channel} onChange={(e) => change('channel', e.target.value)}>
            <option value="web">网页</option>
            <option value="feishu">本人飞书</option>
          </select>
        </label>
        <label>
          时区
          <input
            required
            value={form.timezone}
            onChange={(e) => change('timezone', e.target.value)}
          />
        </label>
        <label className="todo-full">
          处理说明{' '}
          <span className="todo-optional">{form.kind === 'execute' ? '必填' : '可选'}</span>
          <textarea
            rows={2}
            maxLength={4000}
            required={form.kind === 'execute'}
            value={form.instruction}
            onChange={(e) => change('instruction', e.target.value)}
            placeholder={
              form.kind === 'execute'
                ? '希望整理哪些资料，得到什么结果？'
                : '提醒内容或回访时想确认的事'
            }
          />
        </label>
      </div>
      <p className="field-note">
        {form.kind === 'execute'
          ? '读取已有资料与 Linear 后生成报告，不执行外部修改。'
          : form.kind === 'checkin'
            ? '遵循静默、冷却和近期交流规则，可能延后或跳过。'
            : '按约定时间提醒，不受回访静默设置影响。'}
      </p>
      <details className="todo-disclosure">
        <summary>结束时间与补发规则</summary>
        <div className="todo-form-grid">
          <label className="todo-full">
            停止日期 <span className="todo-optional">可选 · 浏览器时区</span>
            <input
              type="datetime-local"
              value={
                form.ends_at
                  ? localTime(form.ends_at, Intl.DateTimeFormat().resolvedOptions().timeZone)
                  : ''
              }
              onChange={(e) =>
                change('ends_at', e.target.value ? new Date(e.target.value).toISOString() : null)
              }
            />
          </label>
          <label>
            错过周期时
            <select
              value={form.missed_policy}
              onChange={(e) => change('missed_policy', e.target.value)}
            >
              <option value="latest">补最近一期</option>
              <option value="skip">跳过错过的周期</option>
            </select>
          </label>
          <label>
            补发宽限（分钟）
            <input
              type="number"
              required
              min={1}
              max={10080}
              value={form.grace_minutes}
              onChange={(e) => change('grace_minutes', Number(e.target.value))}
            />
          </label>
        </div>
        <p className="field-note">周期按当地钟点计算；每月遇到短月份取月末。</p>
      </details>
      <div className="todo-form-actions">
        <button className="primary-button" disabled={busy}>
          {busy ? '保存中…' : '保存安排'}
        </button>
        <button className="secondary-button" type="button" disabled={busy} onClick={cancel}>
          取消
        </button>
      </div>
    </form>
  );
}
