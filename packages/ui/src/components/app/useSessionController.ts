import { SessionCatalogClient } from "../../lib/sessionCatalogClient";
import { sortSessionCatalog as sortSessions } from "../sidebarSessions";
import { t } from "../../i18n";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { UiSession } from "../../types/ui";
import {
  activateSession,
  deleteSession,
  querySessionCatalog,
  reorderSessions,
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


export function useSessionController({
  activeWorkspaceRoot,
  reportError,
}: {
  activeWorkspaceRoot: string | null;
  reportError: (message: string) => void;
}) {
  const catalogRef = useRef<SessionCatalogClient | null>(null);
  if (!catalogRef.current) catalogRef.current = new SessionCatalogClient(querySessionCatalog);
  const catalog = catalogRef.current;
  const [catalogRevision, setCatalogRevision] = useState(0);
  const [sessions, setSessions] = useState<UiSession[]>([]);
  const [currentSessionId, setCurrentSessionId] = useState<string | null>(null);
  const [runningSessionIds, setRunningSessionIds] = useState<Set<string>>(new Set());
  const [completedSessionIds, setCompletedSessionIds] = useState<Set<string>>(new Set());
  const inputsRef = useRef({ activeWorkspaceRoot, reportError });
  const sessionsRef = useRef(sessions);
  const currentSessionIdRef = useRef(currentSessionId);
  const selectionEpochRef = useRef(0);
  const refreshRequestIdRef = useRef(0);
  const catalogMutationRef = useRef(0);
  const pendingPins = useRef(new Set<string>());
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
      load: () => catalog.initialize(),
      applySessions: (items: SessionItem[], preferredSessionId?: string | null): boolean => {
        if (!isCurrent()) return false;
        catalogMutationRef.current += 1;
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
  }, [catalog, setCurrentSession]);

  const refresh = useCallback(async (preferredSessionId?: string | null) => {
    const requestId = refreshRequestIdRef.current + 1;
    refreshRequestIdRef.current = requestId;
    const selectionEpoch = selectionEpochRef.current;
    const selectedSessionId = currentSessionIdRef.current;
    const items = (await catalog.sync()).map(toUiSession);
    const selected = sessionsRef.current.find(item => item.id === currentSessionIdRef.current);
    if (selected && !items.some(item => item.id === selected.id)) {
      const found = (await querySessionCatalog({mode:"lookup",sessionId:selected.id})).items[0];
      if (found) items.push(toUiSession(found));
    }
    const fetched = sortSessions(items);
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
  }, [catalog, setCurrentSession]);

  useEffect(() => {
    if (typeof window === "undefined" || typeof document === "undefined") return;
    let disposed = false;
    let pending = false;
    const discover = async () => {
      if (disposed || pending || pendingPins.current.size || document.visibilityState === "hidden") return;
      pending = true;
      const mutation = catalogMutationRef.current;
      const requestId = ++refreshRequestIdRef.current;
      try {
        const items = (await catalog.sync()).map(toUiSession);
    const selected = sessionsRef.current.find(item => item.id === currentSessionIdRef.current);
    if (selected && !items.some(item => item.id === selected.id)) {
      const found = (await querySessionCatalog({mode:"lookup",sessionId:selected.id})).items[0];
      if (found) items.push(toUiSession(found));
    }
    const fetched = sortSessions(items);
        if (disposed || mutation !== catalogMutationRef.current || requestId !== refreshRequestIdRef.current) return;
        const previous = sessionsRef.current;
        const unchanged = previous.length === fetched.length && fetched.every((session, index) =>
          (Object.keys(session) as (keyof UiSession)[]).every((key) => session[key] === previous[index][key]),
        );
        if (!unchanged) {
          sessionsRef.current = fetched;
          setSessions(fetched);
        }
        // Discovery updates the catalog only: never select another session or erase a draft.
      } catch (error) {
        if (!disposed) inputsRef.current.reportError(errorMessage(error, t("app.unableToLoadConversations")));
      } finally {
        pending = false;
      }
    };
    const onVisible = () => { void discover(); };
    const timer = setInterval(onVisible, 5_000);
    window.addEventListener("focus", onVisible);
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      disposed = true;
      clearInterval(timer);
      window.removeEventListener("focus", onVisible);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [catalog]);

  const selectSession = useCallback(async (
    sessionId: string,
  ): Promise<SessionSelectionResult | null> => {
    const selectionEpoch = selectionEpochRef.current + 1;
    selectionEpochRef.current = selectionEpoch;
    let target = sessionsRef.current.find((session) => session.id === sessionId);
    if (!target) {
      try {
        const fetched = sortSessions((await querySessionCatalog({ mode: "lookup", sessionId })).items.map(toUiSession));
        if (selectionEpochRef.current !== selectionEpoch) return null;
        const merged = sortSessions([...sessionsRef.current.filter(i => i.id !== sessionId), ...fetched]);
        sessionsRef.current = merged;
        setSessions(merged);
        target = fetched.find((session) => session.id === sessionId);
      } catch (error) {
        if (selectionEpochRef.current === selectionEpoch) {
          inputsRef.current.reportError(errorMessage(error, t("app.unableToLoadConversations")));
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
          inputsRef.current.reportError(errorMessage(error, t("useSessionController.unableToSwitchTheConversationWorkspace")));
        }
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
      catalogMutationRef.current += 1;
      setSessions((items) => sortSessions(
        items.map((session) => session.id === sessionId ? updated : session),
      ));
    } catch (error) {
      inputsRef.current.reportError(errorMessage(error, t("useSessionController.unableToRenameConversation")));
      throw error;
    }
  }, []);

  const pinSession = useCallback(async (sessionId: string, isPinned: boolean) => {
    if (pendingPins.current.has(sessionId)) return;
    const previous = sessionsRef.current.find(item => item.id === sessionId);
    if (!previous) return;
    pendingPins.current.add(sessionId);
    const apply = (item: UiSession) => {
      catalogMutationRef.current += 1;
      const next = sortSessions(sessionsRef.current.map(s => s.id === sessionId ? item : s));
      sessionsRef.current = next;
      setSessions(next);
    };
    apply({ ...previous, isPinned });
    try {
      apply(toUiSession(await updateSession(sessionId, { isPinned })));
    } catch (error) {
      // A timed-out write may already be durable. Observe once; never resend it.
      try {
        const observed = (await querySessionCatalog({ mode: "lookup", sessionId })).items[0];
        if (observed) {
          apply(toUiSession(observed));
          if (Boolean(observed.isPinned) === isPinned) return;
        } else apply(previous);
      } catch { apply(previous); }
      inputsRef.current.reportError(errorMessage(error, "Unable to verify pinned chat"));
      throw error;
    } finally {
      pendingPins.current.delete(sessionId);
    }
  }, []);

  const reorderPinnedSessions = useCallback(async (ids: string[]) => {
    const pinned = sessionsRef.current.filter(session => session.isPinned && session.sessionKind === "main");
    if (pendingPins.current.size || ids.length !== pinned.length || new Set(ids).size !== ids.length || ids.some(id => !pinned.some(session => session.id === id))) {
      const error = new Error("Pinned chats changed; try sorting again");
      inputsRef.current.reportError(error.message);
      throw error;
    }
    const previousOrders = new Map(pinned.map(session => [session.id, session.sortOrder]));
    const apply = (next: UiSession[]) => {
      catalogMutationRef.current += 1;
      sessionsRef.current = sortSessions(next);
      setSessions(sessionsRef.current);
    };
    ids.forEach(id => pendingPins.current.add(id));
    apply(sessionsRef.current.map(session => ids.includes(session.id) ? { ...session, sortOrder: ids.indexOf(session.id) } : session));
    try {
      const saved = new Map((await reorderSessions("pinned", ids)).map(item => [item.id, toUiSession(item)]));
      apply(sessionsRef.current.map(session => saved.get(session.id) ?? session));
    } catch (error) {
      try {
        const observed = (await catalog.initialize()).map(toUiSession);
        apply(observed);
        const actual = sortSessions(observed).filter(session => session.isPinned && session.sessionKind === "main").map(session => session.id);
        if (actual.length === ids.length && actual.every((id, index) => id === ids[index])) return;
      } catch {
        apply(sessionsRef.current.map(session => previousOrders.has(session.id) ? { ...session, sortOrder: previousOrders.get(session.id) } : session));
      }
      inputsRef.current.reportError(errorMessage(error, "Unable to save pinned order"));
      throw error;
    } finally {
      ids.forEach(id => pendingPins.current.delete(id));
    }
  }, [catalog]);

  const removeSession = useCallback(async (sessionId: string): Promise<ReadonlySet<string>> => {
    try {
      const currentSessions = sessionsRef.current;
      const target = currentSessions.find((session) => session.id === sessionId);
      if (!target) throw new Error(t("useSessionController.conversationToDeleteDoesNotExistValue", { value1: sessionId }));
      const response = await deleteSession(sessionId);
      if (response.deletedSessionId !== sessionId) {
        throw new Error(t("useSessionController.deleteResponseIdentityMismatchValue", { value1: response.deletedSessionId }));
      }
      if (!Array.isArray(response.deletedSessionIds) || !response.deletedSessionIds.includes(sessionId)) {
        throw new Error("Delete response missing deleted Session identities");
      }
      const deletedIds = new Set(response.deletedSessionIds);
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
      inputsRef.current.reportError(errorMessage(error, t("useSessionController.unableToDeleteConversation")));
      throw error;
    }
  }, [refresh]);

  const resolveSession = useCallback((session: UiSession, options?: { activate?: boolean }) => {
    catalogMutationRef.current += 1;
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

  const expandWorkspace = useCallback(async (root: string) => {
    try {
      const items = await catalog.expand(root);
      setSessions(previous => sortSessions([...previous.filter(p => !items.some(i => i.id === p.id)), ...items.map(toUiSession)]));
      setCatalogRevision(value => value + 1);
    } catch(error) { inputsRef.current.reportError(errorMessage(error, "Unable to load chats")); }
  }, [catalog]);
  const loadMore = useCallback(async (root: string) => {
    try {
      const items = await catalog.more(root);
      setSessions(previous => sortSessions([...previous.filter(p => !items.some(i => i.id === p.id)), ...items.map(toUiSession)]));
      setCatalogRevision(value => value + 1);
    } catch(error) { inputsRef.current.reportError(errorMessage(error, "Unable to load chats")); }
  }, [catalog]);
  useEffect(() => { if (activeWorkspaceRoot) void expandWorkspace(activeWorkspaceRoot); }, [activeWorkspaceRoot, expandWorkspace]);

  const actions = useMemo(() => ({
    expandWorkspace,
    loadMore,
    beginInitialization,
    clearSelection,
    completeSession,
    refresh,
    removeSession,
    renameSession,
    pinSession,
    reorderPinnedSessions,
    resolveSession,
    selectSession,
    setRunning,
  }), [
    expandWorkspace,
    loadMore,
    beginInitialization,
    clearSelection,
    completeSession,
    refresh,
    removeSession,
    renameSession,
    pinSession,
    reorderPinnedSessions,
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
    hasMore: (root: string) => { void catalogRevision; return catalog.hasMore(root); },
    sessions,
    currentSessionId,
    currentSession,
    mainSessions,
    runningSessionIds,
    completedSessionIds,
    actions,
  };
}
