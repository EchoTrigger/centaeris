import { useEffect, useRef } from "react";

// One metadata refresh for the sidebar, independent of the selected Run stream.
export function useSessionNavigationRefresh({ scopeKey, enabled, onRefresh }) {
  const refreshRef = useRef(onRefresh);
  refreshRef.current = onRefresh;
  // biome-ignore lint/correctness/useExhaustiveDependencies: Changing navigation scope must abort the previous list request.
  useEffect(() => {
    if (!enabled || typeof window === "undefined") return undefined;
    const focusTarget = window;
    const visibilityTarget = typeof document === "undefined" ? null : document;
    const controller = new AbortController();
    let timer;
    let busy = false;
    const visible = () => !visibilityTarget?.hidden;
    const schedule = () => {
      clearTimeout(timer);
      if (!controller.signal.aborted && visible()) timer = setTimeout(refresh, 5000);
    };
    async function refresh() {
      clearTimeout(timer);
      if (busy || controller.signal.aborted || !visible()) return;
      busy = true;
      try {
        await refreshRef.current(controller.signal);
      } catch {
        // Keep the last metadata during a transient failure and retry next time.
      } finally {
        busy = false;
        schedule();
      }
    }
    const onVisibilityChange = () => { if (visible()) void refresh(); else clearTimeout(timer); };
    focusTarget.addEventListener("focus", refresh);
    visibilityTarget?.addEventListener("visibilitychange", onVisibilityChange);
    schedule();
    return () => {
      controller.abort();
      clearTimeout(timer);
      focusTarget.removeEventListener("focus", refresh);
      visibilityTarget?.removeEventListener("visibilitychange", onVisibilityChange);
    };
  }, [scopeKey, enabled]);
}
