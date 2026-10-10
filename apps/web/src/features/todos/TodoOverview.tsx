import { useState } from 'react';
import { Check, Pencil } from 'lucide-react';
import { api } from '../../api';
import { TodoEditor } from './TodoEditor';
import { contentOf, STATUS, type Todo } from './types';

/** 默认阅读事项，进入编辑才显示完整表单；完成动作只提交当前已读版本。 */
export function TodoOverview({
  item,
  saved,
  report,
}: {
  item: Todo;
  saved: (id: string, message?: string) => Promise<void>;
  report: (error: unknown) => void;
}) {
  const [editing, setEditing] = useState(false);
  const [busy, setBusy] = useState(false);
  const closed = ['completed', 'cancelled'].includes(item.status);
  async function finish() {
    setBusy(true);
    try {
      const result = await api<{ delivery_may_have_started: boolean }>(`/todos/${item.id}`, {
        method: 'PUT',
        body: JSON.stringify({
          version: item.version,
          content: contentOf(item),
          status: closed ? 'active' : 'completed',
        }),
      });
      await saved(
        item.id,
        closed
          ? '事项已重新打开；按需恢复安排。'
          : result.delivery_may_have_started
            ? '事项已完成；已经开始投递的消息仍可能送达。'
            : '事项已完成，后续安排已停止。',
      );
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  }
  if (editing)
    return (
      <TodoEditor
        item={item}
        cancel={() => setEditing(false)}
        saved={async (id, message) => {
          await saved(id, message);
          setEditing(false);
        }}
        report={report}
      />
    );
  return (
    <section className="settings-card todo-overview">
      <div className="todo-section-heading">
        <span className={`todo-status is-${item.status}`}>{STATUS[item.status]}</span>
        <button className="todo-text-button" onClick={() => setEditing(true)}>
          <Pencil size={14} />
          编辑
        </button>
      </div>
      <h2 className="todo-item-title">{item.title}</h2>
      {item.objective && <p className="todo-objective">{item.objective}</p>}
      <div className="todo-next-action">
        <span>下一步</span>
        <p>{item.next_action || '还没有下一步，可以在编辑中补充。'}</p>
      </div>
      <dl className="todo-facts">
        {item.waiting_on && (
          <div>
            <dt>正在等待</dt>
            <dd>{item.waiting_on}</dd>
          </div>
        )}
        {item.completion_criteria && (
          <div>
            <dt>完成标准</dt>
            <dd>{item.completion_criteria}</dd>
          </div>
        )}
        {item.due_at && (
          <div>
            <dt>截止时间</dt>
            <dd>{new Date(item.due_at).toLocaleString()}</dd>
          </div>
        )}
      </dl>
      <div className="todo-overview-footer">
        <button className="secondary-button" disabled={busy} onClick={() => void finish()}>
          <Check size={15} />
          {closed ? '重新打开' : '完成事项'}
        </button>
        <span className="todo-meta">{closed ? '旧安排保持停止' : '完成后停止全部后续安排'}</span>
      </div>
    </section>
  );
}
