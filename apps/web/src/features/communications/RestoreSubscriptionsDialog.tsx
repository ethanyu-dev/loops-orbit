import { useEffect, useRef } from 'react';
import { X } from 'lucide-react';
import type { RetainedSelection } from './retainedSelection';

/** 确认批量恢复的固定范围，并说明新增采集与历史补录的区别。 */
export function RestoreSubscriptionsDialog({
  sources,
  busy,
  error,
  onClose,
  onConfirm,
}: {
  /** 打开弹窗时冻结的会话名称与版本。 */
  sources: RetainedSelection[];
  /** 执行中禁止重复提交。 */
  busy: boolean;
  /** 版本冲突或网络错误原位显示。 */
  error: string;
  /** 取消不改变订阅。 */
  onClose: () => void;
  /** 只恢复本次确认的范围。 */
  onConfirm: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    const modal = dialog.current;
    modal?.showModal();
    return () => {
      modal?.close();
      previous?.focus();
    };
  }, []);
  return (
    <dialog
      ref={dialog}
      className="subscription-dialog"
      aria-labelledby="restore-subscription-title"
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) onClose();
      }}
    >
      <div className="subscription-dialog-heading">
        <h2 id="restore-subscription-title">重新订阅 {sources.length} 个会话？</h2>
        <button className="icon-button" aria-label="关闭确认弹窗" disabled={busy} onClick={onClose}>
          <X size={18} />
        </button>
      </div>
      <ul className="subscription-selected-names">
        {sources.map((source) => (
          <li key={source.id}>{source.label}</li>
        ))}
      </ul>
      <p>
        解除这些会话的自动订阅排除，从确认时刻开始采集新消息。已有资料保留；停订期间遗漏的消息请通过“整理历史消息”补录。
      </p>
      {error && (
        <p className="subscription-dialog-error" role="alert">
          {error}
        </p>
      )}
      <div className="subscription-dialog-actions">
        <button autoFocus disabled={busy} onClick={onClose}>
          取消
        </button>
        <button className="confirm-removal" disabled={busy} onClick={onConfirm}>
          {busy ? '正在处理…' : '确认重新订阅'}
        </button>
      </div>
    </dialog>
  );
}
