import { useCallback, useEffect, useState } from 'react';
import { api, ApiError, errorText } from '../../api';
import type { Snapshot } from './types';

// 请求完成后再等待十秒；网络挂起也必须在二十秒内退出加载状态。
const POLL_INTERVAL = 10000;
const REQUEST_TIMEOUT = 20000;

/** 状态读取独立恢复；刷新失败保留上次结果，离开页面或主动重试会取消旧请求。 */
export function useCommunicationSnapshot() {
  const [data, setData] = useState<Snapshot | null>(null);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);
  const [revision, setRevision] = useState(0);
  const load = useCallback(async () => setRevision((value) => value + 1), []);
  useEffect(() => {
    let stopped = false;
    let timer: ReturnType<typeof setTimeout>;
    let timeout: ReturnType<typeof setTimeout>;
    let controller: AbortController;
    async function poll() {
      controller = new AbortController();
      let timedOut = false;
      timeout = setTimeout(() => {
        timedOut = true;
        controller.abort();
      }, REQUEST_TIMEOUT);
      setLoading(true);
      try {
        const value = await api<Snapshot>('/communications/status', { signal: controller.signal });
        if (!stopped) {
          setData(value);
          setError('');
        }
      } catch (reason) {
        if (!stopped) setError(errorText(timedOut ? new ApiError(504, 'request_timeout') : reason));
      } finally {
        clearTimeout(timeout);
        if (!stopped) {
          setLoading(false);
          timer = setTimeout(() => void poll(), POLL_INTERVAL);
        }
      }
    }
    void poll();
    return () => {
      stopped = true;
      controller?.abort();
      clearTimeout(timeout);
      clearTimeout(timer);
    };
  }, [revision]);
  return { data, error, loading, load };
}
