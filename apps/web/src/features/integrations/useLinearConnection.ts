import { useCallback, useEffect, useRef, useState } from 'react';
import { api, ApiError, errorText } from '../../api';

/** 只保存可公开的身份与连接状态，前端始终不接触个人密钥。 */
export type LinearStatus = {
  /** 服务端是否已配置个人密钥。 */
  configured: boolean;
  /** 当前密钥验证后的账号；验证成功不代表已确认写权限。 */
  connection: null | {
    /** Linear 返回的实际账号名称。 */
    user_name: string;
    /** 工作空间的可读名称。 */
    workspace_name: string;
    /** 用于核对工作空间的 URL 标识。 */
    workspace_slug: string;
    /** active 表示可用，其他状态提示检查密钥。 */
    status: string;
  };
};

/** 页面内反馈避免与全局错误重复；真正的 Orbit 会话失效仍交给应用处理。 */
type Feedback = {
  /** 决定提示的颜色和辅助技术播报优先级。 */
  tone: 'success' | 'error';
  /** 已转换为用户可读内容，不展示上游原始响应。 */
  text: string;
};

/** 区分读取、验证与断开，防止重复提交和把状态刷新失败误报为操作失败。 */
export function useLinearConnection(report: (error: unknown) => void) {
  const [status, setStatus] = useState<LinearStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [action, setAction] = useState<'verify' | 'disconnect' | null>(null);
  const [feedback, setFeedback] = useState<Feedback | null>(null);
  const pending = useRef(false);
  const mounted = useRef(false);

  /** Linear 密钥失效与 Orbit 登录会话失效分别反馈，避免误退出个人助手。 */
  const handleError = useCallback(
    (error: unknown) => {
      if (error instanceof ApiError && error.status === 401 && error.code !== 'linear_invalid_key')
        report(error);
      setFeedback({ tone: 'error', text: errorText(error) });
    },
    [report],
  );

  /** 重试只读取状态，既不建立连接，也不重新派发此前的写操作。 */
  const refresh = useCallback(async () => {
    if (pending.current) return;
    pending.current = true;
    setLoading(true);
    setFeedback(null);
    try {
      const next = await api<LinearStatus>('/linear/status');
      if (mounted.current) setStatus(next);
    } catch (error) {
      if (mounted.current) handleError(error);
    } finally {
      pending.current = false;
      if (mounted.current) setLoading(false);
    }
  }, [handleError]);

  useEffect(() => {
    mounted.current = true;
    void refresh();
    return () => {
      mounted.current = false;
    };
  }, [refresh]);

  /** 写请求成功后才改变展示；验证后的读取失败只重试读取，不再次派发连接操作。 */
  async function run(nextAction: 'verify' | 'disconnect') {
    if (pending.current) return false;
    pending.current = true;
    setAction(nextAction);
    setFeedback(null);
    try {
      await api('/linear/connection', { method: nextAction === 'verify' ? 'POST' : 'DELETE' });
      if (!mounted.current) return true;
      if (nextAction === 'disconnect') {
        setStatus((current) => current && { ...current, connection: null });
        setFeedback({ tone: 'success', text: '已断开连接，你可以随时重新验证连接。' });
      } else {
        try {
          const next = await api<LinearStatus>('/linear/status');
          if (mounted.current) {
            setStatus(next);
            setFeedback({ tone: 'success', text: '连接状态已刷新。' });
          }
        } catch (error) {
          if (mounted.current) {
            setStatus(null);
            handleError(error);
            setFeedback({
              tone: 'error',
              text: '验证已完成，但暂时无法读取连接状态。请重新加载。',
            });
          }
        }
      }
      return true;
    } catch (error) {
      if (mounted.current) {
        if (error instanceof ApiError && error.code === 'linear_invalid_key') {
          setStatus((current) =>
            current?.connection
              ? { ...current, connection: { ...current.connection, status: 'reauthorize' } }
              : current,
          );
        }
        handleError(error);
      }
      return false;
    } finally {
      pending.current = false;
      if (mounted.current) setAction(null);
    }
  }

  return {
    status,
    loading,
    action,
    feedback,
    refresh,
    run,
    clearFeedback: () => setFeedback(null),
  };
}
