import { useLayoutEffect, useRef, useState } from 'react';

// 只容忍像素取整误差，避免用户靠近底部阅读时被轮询强行拉回。
const BOTTOM_TOLERANCE_PX = 2;

/** 消息区独立管理跟随；内容和输入框改变高度时，仅在原本贴底的情况下跟随。 */
export function useMessageScroll() {
  const viewport = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const following = useRef(true);
  const [atBottom, setAtBottom] = useState(true);

  /** 只修改消息容器，避免 scrollIntoView 连带滚动页面和外层布局。 */
  function scrollToLatest() {
    const view = viewport.current;
    if (!view) return;
    following.current = true;
    view.scrollTop = view.scrollHeight;
    setAtBottom(true);
  }

  /** 用户离开底部立即暂停跟随，回到底部后再恢复。 */
  function onScroll() {
    const view = viewport.current;
    if (!view) return;
    const nearBottom =
      view.scrollHeight - view.scrollTop - view.clientHeight <= BOTTOM_TOLERANCE_PX;
    following.current = nearBottom;
    setAtBottom(nearBottom);
  }

  useLayoutEffect(() => {
    const view = viewport.current;
    const messages = content.current;
    if (!view || !messages) return;
    // 观察真实尺寸变化，不因没有新内容的轮询反复校正滚动位置。
    const observer = new ResizeObserver(() => {
      if (following.current) scrollToLatest();
    });
    scrollToLatest();
    observer.observe(view);
    observer.observe(messages);
    return () => observer.disconnect();
  }, []);

  return { viewport, content, atBottom, onScroll, scrollToLatest };
}
