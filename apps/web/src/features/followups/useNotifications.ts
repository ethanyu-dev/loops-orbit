import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../../api';
import type { Notifications } from './types';

// 网页关闭时不轮询；后台任务和未读状态仍保存在服务端。
const POLL_MS = 10_000;
const EMPTY: Notifications = { unread: 0, items: [] };
/** 切换身份或退出时丢弃旧请求，防止上一身份通知闪回。 */
export function useNotifications(owner: string | undefined, report: (error: unknown) => void) {
  const [data, setData] = useState(EMPTY);
  const generation = useRef(0);
  const refresh = useCallback(async () => {
    if (!owner) return;
    const current = generation.current;
    const next = await api<Notifications>('/notifications');
    if (current === generation.current) setData(next);
  }, [owner]);
  useEffect(() => {
    generation.current += 1;
    setData(EMPTY);
    if (!owner) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        await refresh();
      } catch (error) {
        if (!stopped) report(error);
      }
      if (!stopped) timer = setTimeout(() => void poll(), POLL_MS);
    }
    void poll();
    return () => {
      stopped = true;
      generation.current += 1;
      clearTimeout(timer);
    };
  }, [owner, refresh, report]);
  return { data, refresh };
}
