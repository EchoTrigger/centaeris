import { t } from "./i18n";
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
        <h1>{t("appErrorBoundary.theInterfaceCouldNotBeDisplayed")}</h1>
        <p>{t("appErrorBoundary.thisWindowEncounteredARenderingErrorReloadToRestore")}</p>
        <button type="button" onClick={() => window.location.reload()}>{t("appErrorBoundary.reload")}</button>
      </main>
    );
  }
}
