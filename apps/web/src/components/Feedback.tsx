import type { ReactNode } from 'react';
import { LoaderCircle } from 'lucide-react';

/** 跨页面共用加载指示，统一动画与辅助文字。 */
export function Spinner() {
  return <LoaderCircle size={17} className="spin" aria-label="正在加载" />;
}
/** 保留页面自己的空状态内容，只共享布局容器。 */
export function Empty({ children }: { children: ReactNode }) {
  return <div className="empty-state">{children}</div>;
}
