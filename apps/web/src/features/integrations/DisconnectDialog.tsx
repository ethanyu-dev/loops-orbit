import { useEffect, useRef } from 'react';
import { Unplug } from 'lucide-react';
import { Spinner } from '../../components/Feedback';

/** 原生模态框管理焦点和键盘；请求期间不关闭，失败后返回页面内错误反馈。 */
export function DisconnectDialog({
  open,
  busy,
  onClose,
  onConfirm,
}: {
  /** 打开断开确认，不会立即修改连接。 */
  open: boolean;
  /** 请求已派发时禁用重复确认。 */
  busy: boolean;
  /** 取消或确认结束后恢复页面操作。 */
  onClose: () => void;
  /** 只断开本应用连接，不吊销 Linear 个人密钥。 */
  onConfirm: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const element = dialog.current;
    if (open && element && !element.open) element.showModal();
    if (!open && element?.open) element.close();
  }, [open]);
  return (
    <dialog
      ref={dialog}
      className="integration-dialog"
      aria-labelledby="disconnect-linear-title"
      aria-describedby="disconnect-linear-description"
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) onClose();
      }}
    >
      <div className="integration-dialog-icon">
        <Unplug size={22} aria-hidden="true" />
      </div>
      <h2 id="disconnect-linear-title">断开 Linear 连接？</h2>
      <p id="disconnect-linear-description">
        断开后，Orbit 将无法查询或更新你的 Linear 事项。历史对话会保留，之后可重新连接。
      </p>
      <p className="integration-dialog-note">个人 API Key 不会被撤销，已发出的更新可能仍会完成。</p>
      <div className="integration-dialog-actions">
        <button className="integration-button" autoFocus disabled={busy} onClick={onClose}>
          取消
        </button>
        <button
          className="integration-button integration-button-danger"
          disabled={busy}
          onClick={onConfirm}
        >
          {busy && <Spinner />}
          {busy ? '正在断开…' : '确认断开'}
        </button>
      </div>
    </dialog>
  );
}
