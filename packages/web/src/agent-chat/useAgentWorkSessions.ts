import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError, apiJson } from "../api";
import { agentWorkSessionsPath, parseAgentWorkSessionsPage } from "./agentWorkSessions";
import type { ReferencedSession } from "./agentOutputFeed";

type Window = Readonly<{ scope: string; sessions: readonly ReferencedSession[]; pages: number; next: string | null; hasMore: boolean; loading: boolean; busy: boolean; error: boolean; revision: number }>;
const emptyWindow = (scope: string): Window => ({ scope, sessions: [], pages: 1, next: null, hasMore: false, loading: false, busy: false, error: false, revision: 0 });

export function useAgentWorkSessions(agentId: string, workspaceId: string, enabled: boolean, onAuthorityLost: () => void) {
  const scope = JSON.stringify([agentId, workspaceId]);
  const [window, setWindow] = useState(() => emptyWindow(scope));
  const windowRef = useRef(window); windowRef.current = window;
  const requestRef = useRef<AbortController | null>(null);
  const enabledRef = useRef(enabled); enabledRef.current = enabled;
  const authorityLostRef = useRef(onAuthorityLost); authorityLostRef.current = onAuthorityLost;
  const reset = useCallback(() => { requestRef.current?.abort(); const empty = emptyWindow(scope); windowRef.current = empty; setWindow(empty); }, [scope]);
  const load = useCallback(async (more = false) => {
    if (!enabledRef.current || (more && (windowRef.current.busy || !windowRef.current.hasMore))) return;
    requestRef.current?.abort(); const controller = new AbortController(); requestRef.current = controller;
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
        const page = parseAgentWorkSessionsPage(await apiJson<unknown>(agentWorkSessionsPath(agentId, next), { signal: controller.signal }), agentId, workspaceId, next);
        if (controller.signal.aborted) return;
        for (const session of page.sessions) { if (ids.has(session.sessionId)) throw new Error("agent_work_sessions_duplicate"); ids.add(session.sessionId); sessions.push(session); }
        pages++; next = page.nextAfterSessionId; hasMore = page.hasMore;
        if (!hasMore) break;
      }
      const accepted = { scope, sessions, pages: Math.max(1, pages), next, hasMore, loading: false, busy: false, error: false, revision: previous.revision + 1 };
      windowRef.current = accepted; setWindow(accepted);
    } catch (error) {
      if (controller.signal.aborted) return;
      if (error instanceof ApiError && [401, 403, 404].includes(error.status)) { reset(); authorityLostRef.current(); }
      else { const failed = { ...previous, loading: false, busy: false, error: true }; windowRef.current = failed; setWindow(failed); }
    }
  }, [agentId, workspaceId, scope, reset]);
  useEffect(() => { reset(); return () => requestRef.current?.abort(); }, [reset]);
  useEffect(() => { if (enabled) void load(); return () => requestRef.current?.abort(); }, [enabled, load]);
  useEffect(() => {
    if (!enabled || window.busy || window.error || window.revision === 0) return;
    const timer = setTimeout(() => { void load(); }, 5000);
    return () => clearTimeout(timer);
  }, [enabled, window.busy, window.error, window.revision, load]);
  const current = window.scope === scope ? window : emptyWindow(scope);
  return { ...current, reset, onLoadMore: () => void load(true), onRetry: () => void load() };
}
