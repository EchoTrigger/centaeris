import { useCallback, useMemo, useRef, useState } from "react";
import type { UiSession } from "../../types/ui";
import {
  activateSession,
  deleteSession,
  listSessions,
  updateSession,
  type SessionItem,
} from "../../lib/chatBridge";
import {
  activateWorkspaceRoot,
  type WorkspaceSnapshot,
} from "../../lib/workspaceBridge";
import { sessionViewCacheStore } from "../chat/chatRuntimeCore";

export type SessionSelectionResult = {
  workspaceSnapshot: WorkspaceSnapshot | null;
};

const normalizeRoot = (root?: string | null): string =>
  String(root || "")
    .trim()
    .replace(/^\\\\\?\\/, "")
    .replace(/\\/g, "/")
    .replace(/\/+$/, "")
    .toLowerCase();

const errorMessage = (error: unknown, fallback: string): string =>
  error instanceof Error && error.message.trim()
    ? error.message
    : typeof error === "string" && error.trim()
      ? error
      : fallback;

const toUiSession = (item: SessionItem): UiSession => ({
  id: item.id,
  title: item.title || "New chat",
  summary: item.lastMessage || undefined,
  updatedAt: item.updatedAt,
  isPinned: Boolean(item.isPinned),
  isUnread: Boolean(item.isUnread),
  messageCount: item.messageCount,
  cwd: item.cwd,
  sortOrder: typeof item.sortOrder === "number" ? item.sortOrder : undefined,
  sessionKind: item.sessionKind,
  parentSessionId: item.parentSessionId,
  runtimeJobId: item.runtimeJobId,
});

const sortSessions = (sessions: UiSession[]): UiSession[] =>
  [...sessions].sort((left, right) => {
    if (Boolean(left.isPinned) !== Boolean(right.isPinned)) {
      return left.isPinned ? -1 : 1;
    }
    const leftOrder = left.sortOrder ?? Number.MAX_SAFE_INTEGER;
    const rightOrder = right.sortOrder ?? Number.MAX_SAFE_INTEGER;
    return leftOrder - rightOrder || (right.updatedAt ?? 0) - (left.updatedAt ?? 0);
  });

export function useSessionController({
  activeWorkspaceRoot,
  reportError,
}: {
  activeWorkspaceRoot: string | null;
  reportError: (message: string) => void;
}) {
  const [sessions, setSessions] = useState<UiSession[]>([]);
  const [currentSessionId, setCurrentSessionId] = useState<string | null>(null);
  const [runningSessionIds, setRunningSessionIds] = useState<Set<string>>(new Set());
  const [completedSessionIds, setCompletedSessionIds] = useState<Set<string>>(new Set());
  const inputsRef = useRef({ activeWorkspaceRoot, reportError });
  const sessionsRef = useRef(sessions);
  const currentSessionIdRef = useRef(currentSessionId);
  const selectionEpochRef = useRef(0);
  const refreshRequestIdRef = useRef(0);
  inputsRef.current = { activeWorkspaceRoot, reportError };
  sessionsRef.current = sessions;
  currentSessionIdRef.current = currentSessionId;

  const setCurrentSession = useCallback((sessionId: string | null) => {
    currentSessionIdRef.current = sessionId;
    setCurrentSessionId(sessionId);
  }, []);

  const clearSelection = useCallback(() => {
    selectionEpochRef.current += 1;
    setCurrentSession(null);
  }, [setCurrentSession]);

  const beginInitialization = useCallback(() => {
    const ownerEpoch = selectionEpochRef.current;
    const isCurrent = (): boolean => selectionEpochRef.current === ownerEpoch;
    return {
      isCurrent,
      applySessions: (items: SessionItem[], preferredSessionId?: string | null): boolean => {
        if (!isCurrent()) return false;
        selectionEpochRef.current += 1;
        const mapped = sortSessions(items.map(toUiSession));
        sessionsRef.current = mapped;
        setSessions(mapped);
        const preferred = preferredSessionId
          ? mapped.find(
            (session) => session.id === preferredSessionId && session.sessionKind === "main",
          ) ?? null
          : null;
        setCurrentSession(preferred?.id ?? null);
        return true;
      },
    };
  }, [setCurrentSession]);

  const refresh = useCallback(async (preferredSessionId?: string | null) => {
    const requestId = refreshRequestIdRef.current + 1;
    refreshRequestIdRef.current = requestId;
    const selectionEpoch = selectionEpochRef.current;
    const selectedSessionId = currentSessionIdRef.current;
    const fetched = sortSessions((await listSessions()).map(toUiSession));
    if (refreshRequestIdRef.current !== requestId) return;
    sessionsRef.current = fetched;
    setSessions(fetched);
    if (
      selectionEpochRef.current !== selectionEpoch
      || currentSessionIdRef.current !== selectedSessionId
    ) {
      return;
    }
    const preferred = preferredSessionId
      ? fetched.find(
        (session) => session.id === preferredSessionId && session.sessionKind === "main",
      ) ?? null
      : null;
    const workspaceMatch = fetched.find(
      (session) =>
        session.sessionKind === "main"
        && normalizeRoot(session.cwd) === normalizeRoot(inputsRef.current.activeWorkspaceRoot),
    );
    const next = preferred ?? workspaceMatch ?? null;
    setCurrentSession(next?.id ?? null);
  }, [setCurrentSession]);

  const selectSession = useCallback(async (
    sessionId: string,
  ): Promise<SessionSelectionResult | null> => {
    const selectionEpoch = selectionEpochRef.current + 1;
    selectionEpochRef.current = selectionEpoch;
    let target = sessionsRef.current.find((session) => session.id === sessionId);
    if (!target) {
      try {
        const fetched = sortSessions((await listSessions()).map(toUiSession));
        if (selectionEpochRef.current !== selectionEpoch) return null;
        sessionsRef.current = fetched;
        setSessions(fetched);
        target = fetched.find((session) => session.id === sessionId);
      } catch (error) {
        if (selectionEpochRef.current === selectionEpoch) {
          inputsRef.current.reportError(errorMessage(error, "加载会话失败"));
        }
        return null;
      }
    }
    if (!target || target.sessionKind !== "main") return null;

    let workspaceSnapshot: WorkspaceSnapshot | null = null;
    if (
      target.cwd
      && normalizeRoot(target.cwd) !== normalizeRoot(inputsRef.current.activeWorkspaceRoot)
    ) {
      try {
        const snapshot = await activateWorkspaceRoot(target.cwd);
        if (selectionEpochRef.current !== selectionEpoch) return null;
        workspaceSnapshot = snapshot;
      } catch (error) {
        if (selectionEpochRef.current === selectionEpoch) {
          inputsRef.current.reportError(errorMessage(error, "切换会话工作区失败"));
        }
        return null;
      }
    }
    if (selectionEpochRef.current !== selectionEpoch) return null;

    setCurrentSession(sessionId);
    setCompletedSessionIds((previous) => {
      const next = new Set(previous);
      next.delete(sessionId);
      return next;
    });
    if (target.isUnread) {
      setSessions((previous) => previous.map(
        (session) => session.id === sessionId ? { ...session, isUnread: false } : session,
      ));
      void updateSession(sessionId, { isUnread: false }).catch(() => undefined);
    }
    void activateSession(sessionId, Date.now() * 1_000).catch(() => undefined);
    return { workspaceSnapshot };
  }, [setCurrentSession]);

  const renameSession = useCallback(async (sessionId: string, title: string) => {
    try {
      const updated = toUiSession(await updateSession(sessionId, { title }));
      setSessions((items) => sortSessions(
        items.map((session) => session.id === sessionId ? updated : session),
      ));
    } catch (error) {
      inputsRef.current.reportError(errorMessage(error, "重命名会话失败"));
      throw error;
    }
  }, []);

  const removeSession = useCallback(async (sessionId: string): Promise<ReadonlySet<string>> => {
    try {
      const currentSessions = sessionsRef.current;
      const target = currentSessions.find((session) => session.id === sessionId);
      if (!target) throw new Error(`删除目标会话不存在: ${sessionId}`);
      const response = await deleteSession(sessionId);
      if (response.deletedSessionId !== sessionId) {
        throw new Error(`删除会话响应身份不匹配: ${response.deletedSessionId}`);
      }
      const deletedIds = new Set(
        currentSessions
          .filter((session) => session.id === sessionId || session.parentSessionId === sessionId)
          .map((session) => session.id),
      );
      deletedIds.forEach((id) => sessionViewCacheStore.delete(id));
      setRunningSessionIds((items) => {
        const next = new Set(items);
        deletedIds.forEach((id) => next.delete(id));
        return next;
      });
      setCompletedSessionIds((items) => {
        const next = new Set(items);
        deletedIds.forEach((id) => next.delete(id));
        return next;
      });
      await refresh(
        currentSessionIdRef.current && !deletedIds.has(currentSessionIdRef.current)
          ? currentSessionIdRef.current
          : null,
      );
      return deletedIds;
    } catch (error) {
      inputsRef.current.reportError(errorMessage(error, "删除会话失败"));
      throw error;
    }
  }, [refresh]);

  const resolveSession = useCallback((session: UiSession, options?: { activate?: boolean }) => {
    setSessions((items) => sortSessions([
      session,
      ...items.filter((item) => item.id !== session.id),
    ]));
    if (options?.activate) {
      selectionEpochRef.current += 1;
      setCurrentSession(session.id);
    }
  }, [setCurrentSession]);

  const setRunning = useCallback((sessionId: string, running: boolean) => {
    setRunningSessionIds((previous) => {
      const next = new Set(previous);
      if (running) next.add(sessionId);
      else next.delete(sessionId);
      return next;
    });
  }, []);

  const completeSession = useCallback((sessionId: string) => {
    setRunningSessionIds((previous) => {
      const next = new Set(previous);
      next.delete(sessionId);
      return next;
    });
    if (currentSessionIdRef.current !== sessionId) {
      setCompletedSessionIds((previous) => new Set(previous).add(sessionId));
      setSessions((items) => items.map(
        (session) => session.id === sessionId ? { ...session, isUnread: true } : session,
      ));
    }
    void refresh(currentSessionIdRef.current).catch(() => undefined);
  }, [refresh]);

  const actions = useMemo(() => ({
    beginInitialization,
    clearSelection,
    completeSession,
    refresh,
    removeSession,
    renameSession,
    resolveSession,
    selectSession,
    setRunning,
  }), [
    beginInitialization,
    clearSelection,
    completeSession,
    refresh,
    removeSession,
    renameSession,
    resolveSession,
    selectSession,
    setRunning,
  ]);

  const currentSession = useMemo(
    () => sessions.find((session) => session.id === currentSessionId) ?? null,
    [currentSessionId, sessions],
  );
  const mainSessions = useMemo(
    () => sessions.filter((session) => session.sessionKind === "main"),
    [sessions],
  );

  return {
    sessions,
    currentSessionId,
    currentSession,
    mainSessions,
    runningSessionIds,
    completedSessionIds,
    actions,
  };
}
