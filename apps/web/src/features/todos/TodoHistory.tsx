import { useState } from 'react';
import { api } from '../../api';
import { LinksPanel } from './TodoLinks';
import { STATUS, RUN_STATUS, type Detail } from './types';

// 固定说明不展示内部错误码；详细执行内容由用户主动展开。
const ERRORS: Record<string, string> = {
  identity_changed: '本人绑定已变化',
  todo_schedule_changed: '安排已修改或停止',
  missed_window: '已超出补发窗口',
  schedule_ended: '已到安排结束时间',
  followup_model_failed: '本次处理失败，可重新安排',
  communication_source_changed: '关联资料已变化',
};
const EVENTS: Record<string, string> = {
  created: '创建事项',
  updated: '更新事项',
  scheduled: '安排处理',
  migrated: '迁入旧提醒',
  linked: '关联资料',
  schedule_created: '新增安排',
  schedule_updated: '修改安排',
  schedule_paused: '安排暂停',
  period_completed: '确认本期完成',
};
/** 处理、来源和历史共用紧凑区域，长报告折叠，不影响本期完成入口。 */
export function TodoHistory({
  detail,
  saved,
  report,
}: {
  detail: Detail;
  saved: (message?: string) => Promise<void>;
  report: (error: unknown) => void;
}) {
  const [tab, setTab] = useState('runs');
  const [busy, setBusy] = useState(false);
  async function complete(runId: string, version: number) {
    setBusy(true);
    try {
      await api(`/todos/${detail.item.id}/runs/complete`, {
        method: 'POST',
        body: JSON.stringify({ run_id: runId, version, note: '本人在网页确认本期已完成' }),
      });
      await saved('本期已完成，后续周期继续。');
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  }
  return (
    <section className="settings-card todo-history">
      <div className="todo-history-nav" aria-label="事项记录">
        {[
          ['runs', '处理记录', detail.runs.length],
          ['links', '关联资料', detail.links.length],
          ['events', '变更记录', detail.events.length],
        ].map(([value, label, count]) => (
          <button key={value} aria-pressed={tab === value} onClick={() => setTab(String(value))}>
            {label}
            <span>{count}</span>
          </button>
        ))}
      </div>
      {tab === 'links' && (
        <LinksPanel detail={detail} saved={() => saved('资料已关联')} report={report} />
      )}
      {tab === 'runs' && (
        <div>
          {!detail.runs.length && (
            <p className="todo-empty-small">还没有处理记录。提醒或整理执行后，可在这里查看结果。</p>
          )}
          {detail.runs.map((run) => (
            <article className="todo-run" key={run.id}>
              <div className="todo-row-heading">
                <div>
                  <strong>
                    {run.completed_at ? '本期已完成' : (RUN_STATUS[run.status] ?? run.status)}
                  </strong>
                  <small>{new Date(run.scheduled_at).toLocaleString()}</small>
                </div>
                {!run.completed_at && (
                  <button
                    className="secondary-button"
                    disabled={busy}
                    onClick={() => void complete(run.id, run.version)}
                  >
                    本期完成
                  </button>
                )}
              </div>
              {run.result && (
                <details className="todo-disclosure">
                  <summary>查看处理结果</summary>
                  <p className="todo-result">{run.result}</p>
                </details>
              )}
              {run.error && (
                <p className="field-note">
                  {ERRORS[run.error] ?? '本次处理未完成，请核对安排后重试。'}
                </p>
              )}
              {run.completed_at && <p className="field-note">{run.completion_note}</p>}
            </article>
          ))}
        </div>
      )}
      {tab === 'events' && (
        <div>
          {detail.events.map((event) => (
            <details className="todo-event" key={event.seq}>
              <summary>
                <span>{EVENTS[event.kind] ?? '事项变更'}</span>
                <small>
                  {new Date(event.created_at).toLocaleString()} ·{' '}
                  {event.actor === 'admin'
                    ? '网页本人'
                    : event.actor.startsWith('feishu:')
                      ? '飞书'
                      : '系统'}
                </small>
              </summary>
              <EventDetail detail={event.detail} />
            </details>
          ))}
        </div>
      )}
    </section>
  );
}
/** 时间线展示用户可理解的变更正文，不把内部标识与协议字段放入日常流程。 */
function EventDetail({ detail }: { detail: unknown }) {
  const data = detail && typeof detail === 'object' ? (detail as Record<string, unknown>) : {};
  const content =
    data.content && typeof data.content === 'object'
      ? (data.content as Record<string, unknown>)
      : {};
  return (
    <div>
      {typeof data.status === 'string' && <p>状态：{STATUS[data.status] ?? '已更新'}</p>}
      {Object.entries({
        title: '名称',
        objective: '目标',
        completion_criteria: '完成标准',
        next_action: '下一步',
        waiting_on: '等待事项',
      }).map(([key, label]) =>
        typeof content[key] === 'string' && content[key] ? (
          <p key={key}>
            {label}：{content[key] as string}
          </p>
        ) : null,
      )}
      {typeof data.note === 'string' && <p>{data.note}</p>}
    </div>
  );
}
