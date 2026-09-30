import { Component, type ReactNode } from 'react';

/** 故障边界的内容与恢复信号。 */
interface PageBoundaryProps {
  /** 当前路由页面。 */
  children: ReactNode;
  /** 换页时清除旧故障，健康页面中的在途提交保持原生命周期。 */
  resetKey: string;
}

/** 只保存是否失败，不将内部异常细节显示到页面。 */
interface PageBoundaryState {
  /** 捕获异常后显示重新加载入口。 */
  failed: boolean;
}

/** 路由页的错误边界，保留外层导航，使加载失败或渲染故障可恢复。 */
export class PageBoundary extends Component<PageBoundaryProps, PageBoundaryState> {
  state = { failed: false };

  /** 新发布后旧分块可能不存在；重新加载会重新读取无缓存的入口和资源清单。 */
  static getDerivedStateFromError() {
    return { failed: true };
  }

  /** 在错误状态下换页才重置，健康页面保持原组件生命周期。 */
  componentDidUpdate(previous: Readonly<PageBoundaryProps>) {
    if (previous.resetKey !== this.props.resetKey && this.state.failed)
      this.setState({ failed: false });
  }

  /** 页面故障只替换内容区，不影响退出和其他功能入口。 */
  render() {
    if (this.state.failed)
      return (
        <div className="settings-page" role="alert">
          <h1>页面暂时无法显示</h1>
          <p>请重新加载页面，或从侧栏打开其他功能。</p>
          <button onClick={() => location.reload()}>重新加载</button>
        </div>
      );
    return this.props.children;
  }
}
