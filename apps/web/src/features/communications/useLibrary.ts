import { useEffect, useState } from 'react';
import { api, ApiError, errorText } from '../../api';
import type { DayPage } from './library';

// 目录和搜索请求有界等待，超时后可以原地重试。
const REQUEST_TIMEOUT = 20000;

/** 地址改变即清空旧结果并取消旧请求，避免快速切目录时串页。 */
export function useLibraryRequest<T>(path: string | null, revision: number) {
  const [result, setResult] = useState<{ path: string; revision: number; data: T } | null>(null);
  const [failure, setFailure] = useState<{
    path: string;
    revision: number;
    message: string;
  } | null>(null);
  const [loading, setLoading] = useState(false);
  useEffect(() => {
    if (!path) return;
    const controller = new AbortController();
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      controller.abort();
    }, REQUEST_TIMEOUT);
    setLoading(true);
    setFailure(null);
    void api<T>(path, { signal: controller.signal })
      .then((data) => {
        if (!controller.signal.aborted) setResult({ path, revision, data });
      })
      .catch((error) => {
        if (!controller.signal.aborted || timedOut)
          setFailure({
            path,
            revision,
            message: errorText(timedOut ? new ApiError(504, 'request_timeout') : error),
          });
      })
      .finally(() => {
        clearTimeout(timer);
        if (!controller.signal.aborted || timedOut) setLoading(false);
      });
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [path, revision]);
  return {
    data: result?.path === path && result.revision === revision ? result.data : null,
    error: failure?.path === path && failure.revision === revision ? failure.message : '',
    loading:
      !!path &&
      (loading ||
        ((result?.path !== path || result.revision !== revision) &&
          (failure?.path !== path || failure.revision !== revision))),
  };
}

/** 日期分页只追加已成功读取的目录；加载失败不会丢弃前面的日期。 */
export function useLibraryDays(revision: number) {
  const [before, setBefore] = useState<string | null>(null);
  const [pages, setPages] = useState<DayPage[]>([]);
  const path = `/communications/library/days${before ? `?before=${before}` : ''}`;
  const request = useLibraryRequest<DayPage>(path, revision);
  useEffect(() => {
    setBefore(null);
    setPages([]);
  }, [revision]);
  useEffect(() => {
    if (!request.data) return;
    setPages((previous) =>
      before
        ? [...previous.filter((page) => page !== request.data), request.data!]
        : [request.data!],
    );
  }, [request.data, before]);
  // 相邻分页之间可能发生重新导入，日期按键去重后保持倒序。
  const days = [
    ...new Map(pages.flatMap((page) => page.items).map((day) => [day.day, day])).values(),
  ].sort((a, b) => b.day.localeCompare(a.day));
  const next = pages.at(-1)?.next_before;
  return {
    days,
    loading: request.loading,
    error: request.error,
    more: !!next,
    loadMore: () => {
      if (next) setBefore(next);
    },
  };
}
