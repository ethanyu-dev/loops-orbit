import { useEffect, useRef, useState } from 'react';
import { X } from 'lucide-react';

/** 弹窗捕获打开时的范围，后台轮询不能扩大这次确认的移除对象。 */
export interface RemovalTarget {
  /** 单个来源或全部订阅的版本约束，原样交给服务端校验。 */
  selection:
    | { scope: 'all'; revision: string }
    | { scope: 'one'; id: string; version: number }
    | { scope: 'retained'; sources: { id: string; version: number }[] };
  /** 打开弹窗时的会话数量。 */
  count: number;
  /** 单项会话名称，全部操作时为空。 */
  label?: string;
  /** 批量删除时展示确认范围，不随后台轮询变动。 */
  labels?: string[];
  /** 已移除来源只剩资料清理动作。 */
  deleteOnly?: boolean;
}

/** 原生模态框提供焦点约束和 Escape 行为；删除默认不勾选。 */
export function RemoveSubscriptionDialog({
  target,
  busy,
  error,
  onClose,
  onConfirm,
}: {
  /** 用户打开弹窗时看到的范围。 */
  target: RemovalTarget;
  /** 执行期间禁止重复提交或关闭。 */
  busy: boolean;
  /** 保留弹窗显示服务端拒绝或连接失败。 */
  error: string;
  /** 关闭不会产生任何写请求。 */
  onClose: () => void;
  /** 仅确认按钮提交用户选择。 */
  onConfirm: (deleteDocuments: boolean) => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [deleteDocuments, setDeleteDocuments] = useState(!!target.deleteOnly);
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
      aria-labelledby="remove-subscription-title"
      aria-describedby="remove-subscription-description"
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) onClose();
      }}
      onClick={(event) => {
        if (!busy && event.target === event.currentTarget) {
          const bounds = event.currentTarget.getBoundingClientRect();
          if (
            event.clientX < bounds.left ||
            event.clientX > bounds.right ||
            event.clientY < bounds.top ||
            event.clientY > bounds.bottom
          )
            onClose();
        }
      }}
    >
      <div className="subscription-dialog-heading">
        <h2 id="remove-subscription-title">
          {target.deleteOnly
            ? '删除保留资料？'
            : target.selection.scope === 'all'
              ? '移除全部订阅？'
              : '移除此订阅？'}
        </h2>
        <button className="icon-button" aria-label="关闭确认弹窗" disabled={busy} onClick={onClose}>
          <X size={18} />
        </button>
      </div>
      <p id="remove-subscription-description">
        {target.selection.scope === 'all'
          ? `将移除全部 ${target.count} 个订阅（含已暂停项），不受当前搜索或分页影响。`
          : target.selection.scope === 'retained'
            ? `将删除所选 ${target.count} 个会话的保留资料及本地列表记录。`
            : `会话：${target.label}`}
      </p>
      {target.labels && (
        <ul className="subscription-selected-names">
          {target.labels.map((label, index) => (
            <li key={index}>{label}</li>
          ))}
        </ul>
      )}
      {!target.deleteOnly && (
        <p>
          停止后续同步、历史导入和相关提醒。飞书账号保持连接，可随时重新订阅。已移除私聊不会被自动加回，未来新发现的私聊仍会自动加入。
        </p>
      )}
      {target.deleteOnly ? (
        <p>
          这些会话已经移除订阅。本次删除原文、图片解读、摘要、检索索引及本地列表记录；不会解除自动订阅排除。若要恢复采集，请取消并选择“重新订阅”。
        </p>
      ) : (
        <label className="subscription-delete-option">
          <input
            type="checkbox"
            checked={deleteDocuments}
            disabled={busy || target.deleteOnly}
            onChange={(event) => setDeleteDocuments(event.target.checked)}
          />
          <span>
            <strong>同时删除这些会话的本地资料</strong>
            <small>原文、图片解读、摘要与检索索引</small>
          </span>
        </label>
      )}
      <p className={deleteDocuments ? 'subscription-delete-warning' : 'subscription-keep-note'}>
        {deleteDocuments
          ? '删除后无法撤销，飞书中的原始消息不受影响。'
          : '资料会保留在沟通记录中，移除后不再参与内容检索。'}
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
        <button
          className="confirm-removal"
          disabled={busy}
          onClick={() => onConfirm(deleteDocuments)}
        >
          {busy
            ? '正在处理…'
            : deleteDocuments
              ? target.deleteOnly
                ? '确认删除资料'
                : '确认移除并删除'
              : '确认移除订阅'}
        </button>
      </div>
    </dialog>
  );
}
