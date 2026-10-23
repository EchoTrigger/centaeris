import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError, apiJson } from "../api";
import { useRevisionRefresh } from "../chat/useRevisionRefresh";
import { agentWorkSessionsPath, parseAgentWorkSessionsPage } from "./agentWorkSessions";
import type { ReferencedSession } from "./agentOutputFeed";

type Window = Readonly<{ scope: string; sessions: readonly ReferencedSession[]; pages: number; next: string | null; hasMore: boolean; loading: boolean; busy: boolean; error: boolean; revision: number }>;
const emptyWindow = (scope: string): Window => ({ scope, sessions: [], pages: 1, next: null, hasMore: false, loading: false, busy: false, error: false, revision: 0 });

export function useAgentWorkSessions(agentId: string, workspaceId: string, enabled: boolean, onAuthorityLost: () => void, workRevision = "0") {
  const scope = JSON.stringify([agentId, workspaceId]);
  const [window, setWindow] = useState(() => emptyWindow(scope));
  const windowRef = useRef(window); windowRef.current = window;
  const requestRef = useRef<AbortController | null>(null);
  const completionRef = useRef(Promise.resolve());
  const enabledRef = useRef(enabled); enabledRef.current = enabled;
  const authorityLostRef = useRef(onAuthorityLost); authorityLostRef.current = onAuthorityLost;
  const reset = useCallback(() => { requestRef.current?.abort(); const empty = emptyWindow(scope); windowRef.current = empty; setWindow(empty); }, [scope]);
  const load = useCallback(async (more = false, invalidationSignal?: AbortSignal) => {
    if (!enabledRef.current || (more && (windowRef.current.busy || !windowRef.current.hasMore))) return;
    while (windowRef.current.busy && !more && !invalidationSignal?.aborted) await completionRef.current;
    if (!enabledRef.current || invalidationSignal?.aborted) return;
    let complete!: () => void;
    completionRef.current = new Promise<void>(resolve => { complete = resolve; });
    requestRef.current?.abort(); const controller = new AbortController(); requestRef.current = controller;
    const signal = invalidationSignal ? AbortSignal.any([controller.signal, invalidationSignal]) : controller.signal;
    const previous = windowRef.current.scope === scope ? windowRef.current : emptyWindow(scope);
    const loading = { ...previous, loading: more || previous.revision === 0, busy: true, error: false }; windowRef.current = loading; setWindow(loading);
    try {
      // A refresh visits exactly the previously opened window. hasMore never
      // expands it; only a user's explicit action adds one bounded page.
      const pageBound = more ? 1 : previous.pages;
      let next = more ? previous.next : null; let hasMore = false; let pages = more ? previous.pages : 0;
      const sessions = more ? [...previous.sessions] : [];
      const ids = new Set(sessions.map(s => s.sessionId));
      for (let index = 0; index < pageBound; index++) {
        const page = parseAgentWorkSessionsPage(await apiJson<unknown>(agentWorkSessionsPath(agentId, next), { signal }), agentId, workspaceId, next);
        if (signal.aborted) return;
        for (const session of page.sessions) { if (ids.has(session.sessionId)) throw new Error("agent_work_sessions_duplicate"); ids.add(session.sessionId); sessions.push(session); }
        pages++; next = page.nextAfterSessionId; hasMore = page.hasMore;
        if (!hasMore) break;
      }
      const accepted = { scope, sessions, pages: Math.max(1, pages), next, hasMore, loading: false, busy: false, error: false, revision: previous.revision + 1 };
      windowRef.current = accepted; setWindow(accepted);
    } catch (error) {
      if (signal.aborted) return;
      if (error instanceof ApiError && [401, 403, 404].includes(error.status)) { reset(); authorityLostRef.current(); }
      else { const failed = { ...previous, loading: false, busy: false, error: true }; windowRef.current = failed; setWindow(failed); }
    } finally { complete(); }
  }, [agentId, workspaceId, scope, reset]);
  useEffect(() => { reset(); return () => requestRef.current?.abort(); }, [reset]);
  useRevisionRefresh(scope, workRevision, enabled, signal => load(false, signal));
  useEffect(() => {
    if (!enabled) {
      requestRef.current?.abort();
      const previous = windowRef.current;
      if (previous.busy) {
        const paused = { ...previous, busy: false, loading: false };
        windowRef.current = paused; setWindow(paused);
      }
    }
  }, [enabled]);
  const current = window.scope === scope ? window : emptyWindow(scope);
  return { ...current, reset, onLoadMore: () => void load(true), onRetry: () => void load() };
}
