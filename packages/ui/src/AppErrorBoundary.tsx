import { Component, type ErrorInfo, type ReactNode } from "react";

type AppErrorBoundaryProps = {
  children: ReactNode;
};

type AppErrorBoundaryState = {
  failed: boolean;
};

export class AppErrorBoundary extends Component<
  AppErrorBoundaryProps,
  AppErrorBoundaryState
> {
  state: AppErrorBoundaryState = { failed: false };

  static getDerivedStateFromError(): AppErrorBoundaryState {
    return { failed: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("Centaeris UI render failed", error, info);
  }

  render() {
    if (!this.state.failed) {
      return this.props.children;
    }
    return (
      <main className="appFatalError" role="alert">
        <h1>界面暂时无法显示</h1>
        <p>当前窗口遇到了渲染错误。重新载入可以恢复到最近保存的状态。</p>
        <button type="button" onClick={() => window.location.reload()}>
          重新载入
        </button>
      </main>
    );
  }
}
