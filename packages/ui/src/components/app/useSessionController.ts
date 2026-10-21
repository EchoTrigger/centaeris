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
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";
import { createSessionCatalogViewStore } from "./sessionCatalogViewStore";

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
  const [store] = useState(createSessionCatalogViewStore);
  const getViewState = store.getState;
  const { catalogRevision, sessions, currentSessionId, runningSessionIds, completedSessionIds } = useStore(
    store,
    useShallow(state => ({
      catalogRevision: state.catalogRevision,
      sessions: state.sessions,
      currentSessionId: state.currentSessionId,
      runningSessionIds: state.runningSessionIds,
      completedSessionIds: state.completedSessionIds,
    })),
  );
  const { setSessions, setCurrentSession, setRunningSessionIds, setCompletedSessionIds, advanceCatalogRevision } = getViewState();
  const inputsRef = useRef({ activeWorkspaceRoot, reportError });
  const selectionEpochRef = useRef(0);
  const refreshRequestIdRef = useRef(0);
  const catalogMutationRef = useRef(0);
  const pendingPins = useRef(new Set<string>());
  inputsRef.current = { activeWorkspaceRoot, reportError };

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
  }, [catalog, setCurrentSession, setSessions]);

  const refresh = useCallback(async (preferredSessionId?: string | null) => {
    const requestId = refreshRequestIdRef.current + 1;
    refreshRequestIdRef.current = requestId;
    const selectionEpoch = selectionEpochRef.current;
    const selectedSessionId = getViewState().currentSessionId;
    const items = (await catalog.sync()).map(toUiSession);
    const view = getViewState();
    const selected = view.sessions.find(item => item.id === view.currentSessionId);
    if (selected && !items.some(item => item.id === selected.id)) {
      const found = (await querySessionCatalog({mode:"lookup",sessionId:selected.id})).items[0];
      if (found) items.push(toUiSession(found));
    }
    const fetched = sortSessions(items);
    if (refreshRequestIdRef.current !== requestId) return;
    setSessions(fetched);
    if (
      selectionEpochRef.current !== selectionEpoch
      || getViewState().currentSessionId !== selectedSessionId
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
  }, [catalog, setCurrentSession, getViewState, setSessions]);

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
        const view = getViewState();
        const selected = view.sessions.find(item => item.id === view.currentSessionId);
        if (selected && !items.some(item => item.id === selected.id)) {
          const found = (await querySessionCatalog({ mode: "lookup", sessionId: selected.id })).items[0];
          if (found) items.push(toUiSession(found));
        }
        const fetched = sortSessions(items);
        if (disposed || mutation !== catalogMutationRef.current || requestId !== refreshRequestIdRef.current) return;
        const previous = getViewState().sessions;
        const unchanged = previous.length === fetched.length && fetched.every((session, index) =>
          (Object.keys(session) as (keyof UiSession)[]).every((key) => session[key] === previous[index][key]),
        );
        if (!unchanged) {
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
  }, [catalog, setSessions, getViewState]);

  const selectSession = useCallback(async (
    sessionId: string,
  ): Promise<SessionSelectionResult | null> => {
    const selectionEpoch = selectionEpochRef.current + 1;
    selectionEpochRef.current = selectionEpoch;
    let target = getViewState().sessions.find((session) => session.id === sessionId);
    if (!target) {
      try {
        const fetched = sortSessions((await querySessionCatalog({ mode: "lookup", sessionId })).items.map(toUiSession));
        if (selectionEpochRef.current !== selectionEpoch) return null;
        const merged = sortSessions([...getViewState().sessions.filter(i => i.id !== sessionId), ...fetched]);
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
  }, [setCurrentSession, getViewState, setCompletedSessionIds, setSessions]);

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
  }, [setSessions]);

  const pinSession = useCallback(async (sessionId: string, isPinned: boolean) => {
    if (pendingPins.current.has(sessionId)) return;
    const previous = getViewState().sessions.find(item => item.id === sessionId);
    if (!previous) return;
    pendingPins.current.add(sessionId);
    const apply = (item: UiSession) => {
      catalogMutationRef.current += 1;
      const next = sortSessions(getViewState().sessions.map(s => s.id === sessionId ? item : s));
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
  }, [getViewState, setSessions]);

  const reorderPinnedSessions = useCallback(async (ids: string[]) => {
    const pinned = getViewState().sessions.filter(session => session.isPinned && session.sessionKind === "main");
    if (pendingPins.current.size || ids.length !== pinned.length || new Set(ids).size !== ids.length || ids.some(id => !pinned.some(session => session.id === id))) {
      const error = new Error("Pinned chats changed; try sorting again");
      inputsRef.current.reportError(error.message);
      throw error;
    }
    const previousOrders = new Map(pinned.map(session => [session.id, session.sortOrder]));
    const apply = (next: UiSession[]) => {
      catalogMutationRef.current += 1;
      setSessions(sortSessions(next));
    };
    ids.forEach(id => pendingPins.current.add(id));
    apply(getViewState().sessions.map(session => ids.includes(session.id) ? { ...session, sortOrder: ids.indexOf(session.id) } : session));
    try {
      const saved = new Map((await reorderSessions("pinned", ids)).map(item => [item.id, toUiSession(item)]));
      apply(getViewState().sessions.map(session => saved.get(session.id) ?? session));
    } catch (error) {
      try {
        const observed = (await catalog.initialize()).map(toUiSession);
        apply(observed);
        const actual = sortSessions(observed).filter(session => session.isPinned && session.sessionKind === "main").map(session => session.id);
        if (actual.length === ids.length && actual.every((id, index) => id === ids[index])) return;
      } catch {
        apply(getViewState().sessions.map(session => previousOrders.has(session.id) ? { ...session, sortOrder: previousOrders.get(session.id) } : session));
      }
      inputsRef.current.reportError(errorMessage(error, "Unable to save pinned order"));
      throw error;
    } finally {
      ids.forEach(id => pendingPins.current.delete(id));
    }
  }, [catalog, getViewState, setSessions]);

  const removeSession = useCallback(async (sessionId: string): Promise<ReadonlySet<string>> => {
    try {
      const currentSessions = getViewState().sessions;
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
      const selectedSessionId = getViewState().currentSessionId;
      await refresh(selectedSessionId && !deletedIds.has(selectedSessionId) ? selectedSessionId : null);
      return deletedIds;
    } catch (error) {
      inputsRef.current.reportError(errorMessage(error, t("useSessionController.unableToDeleteConversation")));
      throw error;
    }
  }, [refresh, getViewState, setRunningSessionIds, setCompletedSessionIds]);

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
  }, [setCurrentSession, setSessions]);

  const setRunning = useCallback((sessionId: string, running: boolean) => {
    setRunningSessionIds((previous) => {
      const next = new Set(previous);
      if (running) next.add(sessionId);
      else next.delete(sessionId);
      return next;
    });
  }, [setRunningSessionIds]);

  const completeSession = useCallback((sessionId: string) => {
    setRunningSessionIds((previous) => {
      const next = new Set(previous);
      next.delete(sessionId);
      return next;
    });
    if (getViewState().currentSessionId !== sessionId) {
      setCompletedSessionIds((previous) => new Set(previous).add(sessionId));
      setSessions((items) => items.map(
        (session) => session.id === sessionId ? { ...session, isUnread: true } : session,
      ));
    }
    void refresh(getViewState().currentSessionId).catch(() => undefined);
  }, [refresh, getViewState, setSessions, setRunningSessionIds, setCompletedSessionIds]);

  const expandWorkspace = useCallback(async (root: string) => {
    try {
      const items = await catalog.expand(root);
      setSessions(previous => sortSessions([...previous.filter(p => !items.some(i => i.id === p.id)), ...items.map(toUiSession)]));
      advanceCatalogRevision();
    } catch(error) { inputsRef.current.reportError(errorMessage(error, "Unable to load chats")); }
  }, [catalog, setSessions, advanceCatalogRevision]);
  const loadMore = useCallback(async (root: string) => {
    try {
      const items = await catalog.more(root);
      setSessions(previous => sortSessions([...previous.filter(p => !items.some(i => i.id === p.id)), ...items.map(toUiSession)]));
      advanceCatalogRevision();
    } catch(error) { inputsRef.current.reportError(errorMessage(error, "Unable to load chats")); }
  }, [catalog, setSessions, advanceCatalogRevision]);
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
