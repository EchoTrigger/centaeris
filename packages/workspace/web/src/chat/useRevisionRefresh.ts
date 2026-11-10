import { useCallback, useEffect, useRef } from "react";

const visible = () => typeof document === "undefined" || document.visibilityState !== "hidden";

/** Refresh a bounded read on authoritative invalidation, never on a timer.
 * Revisions arriving during a request collapse into one following read. A
 * visibility transition calibrates once because delivery may pause in a tab.
 */
export function useRevisionRefresh(
  scope: string, revision: string, enabled: boolean, refresh: (signal: AbortSignal) => Promise<void>,
) {
  const latest = useRef({ scope, revision, refresh }); latest.current = { scope, revision, refresh };
  const scheduleRef = useRef<(force?: boolean, observedRevision?: string) => void>(() => {});
  useEffect(() => {
    if (!enabled) return;
    const owner = scope;
    let active = true;
    let running = false;
    let pending = true;
    let attempted = latest.current.revision;
    const controller = new AbortController();
    const drain = async () => {
      if (!active || latest.current.scope !== owner || running || !visible()) return;
      running = true;
      try {
        while (active && latest.current.scope === owner && pending && visible()) {
          pending = false;
          attempted = latest.current.revision;
          try { await latest.current.refresh(controller.signal); }
          catch { /* The owning view renders errors and offers explicit retry. */ }
        }
      } finally { running = false; }
    };
    const schedule = (force = false, observedRevision = latest.current.revision) => {
      if (force || attempted !== observedRevision) pending = true;
      void drain();
    };
    scheduleRef.current = schedule;
    const onVisible = () => { if (visible()) schedule(true); };
    if (typeof document !== "undefined") document.addEventListener("visibilitychange", onVisible);
    void drain();
    return () => {
      active = false;
      controller.abort();
      if (scheduleRef.current === schedule) scheduleRef.current = () => {};
      if (typeof document !== "undefined") document.removeEventListener("visibilitychange", onVisible);
    };
  }, [scope, enabled]);
  useEffect(() => { scheduleRef.current(false, revision); }, [revision]);
  return useCallback(() => { scheduleRef.current(true); }, []);
}
