import type { TerminalSnapshot } from "../../lib/terminalBridge";
import { t } from "../../i18n";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { UiSession } from "../../types/ui";
import { readDesktopFilePreview, type WorkspaceFileTreeEntry } from "../../lib/workspaceBridge";
import type { SummaryPanelTab } from "../SummaryPanel";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";
import { createWorkspacePanelStore } from "./workspacePanelStore";

type OpenWorkspacePathOptions = {
  refresh?: boolean;
  startLine?: number;
  endLine?: number;
  taskId?: string;
};

const normalizeRoot = (root?: string | null): string =>
  String(root || "")
    .trim()
    .replace(/^\\\\\?\\/, "")
    .replace(/\\/g, "/")
    .replace(/\/+$/, "")
    .toLowerCase();

const fileName = (path: string): string => path.split(/[\\/]/).filter(Boolean).at(-1) || path;

export function useWorkspacePanelController({
  workspaceRoot,
  sessions,
  currentSessionId,
}: {
  workspaceRoot: string | null;
  sessions: UiSession[];
  currentSessionId: string | null;
}) {
  const [store] = useState(createWorkspacePanelStore);
  const getPanelState = store.getState;
  const state = useStore(store, useShallow(({ tabs, activeTabId, isOpen }) => ({ tabs, activeTabId, isOpen })));
  const { dispatch, saveScope } = getPanelState();
  const inputsRef = useRef({ workspaceRoot, sessions, currentSessionId });
  const previewRequestIdsRef = useRef(new Map<string, number>());
  const nextPreviewRequestIdRef = useRef(0);
  inputsRef.current = { workspaceRoot, sessions, currentSessionId };

  const clear = useCallback(() => {
    previewRequestIdsRef.current.clear();
    dispatch({ type: "clear" });
  }, [dispatch]);

  const restoreScope = useCallback((key: string) => {
    previewRequestIdsRef.current.clear();
    getPanelState().restoreScope(key);
  }, [getPanelState]);

  const collapse = useCallback(() => {
    dispatch({ type: "collapse" });
  }, [dispatch]);

  const show = useCallback(() => {
    dispatch({ type: "show" });
  }, [dispatch]);

  const selectTab = useCallback((tabId: string) => {
    dispatch({ type: "select", tabId });
  }, [dispatch]);

  const closeTab = useCallback((tabId: string) => {
    previewRequestIdsRef.current.delete(tabId);
    dispatch({ type: "close-tab", tabId });
  }, [dispatch]);

  const removeAgentSessions = useCallback((sessionIds: ReadonlySet<string>) => {
    dispatch({ type: "remove-agent-sessions", sessionIds });
  }, [dispatch]);

  const openFilePath = useCallback(async (path: string, options?: OpenWorkspacePathOptions) => {
    const normalizedPath = path.trim();
    const activeWorkspaceRoot = inputsRef.current.workspaceRoot;
    if (!normalizedPath || !activeWorkspaceRoot) return;
    const tabId = `file:${normalizeRoot(activeWorkspaceRoot)}:${normalizedPath.replace(/\\/g, "/").toLowerCase()}`;
    const existing = getPanelState().tabs.find((tab) => tab.id === tabId);
    if (existing && !existing.error && !options?.refresh && !options?.startLine && !options?.endLine) {
      dispatch({ type: "browse-file", tabId });
      return;
    }
    const loadingTab: SummaryPanelTab = {
      ...existing,
      id: tabId,
      kind: "file",
      workspaceRoot: activeWorkspaceRoot,
      title: fileName(normalizedPath),
      path: normalizedPath,
      targetLine: options?.startLine,
      targetEndLine: options?.endLine,
      loading: !existing,
      error: undefined,
    };
    const requestId = nextPreviewRequestIdRef.current + 1;
    nextPreviewRequestIdRef.current = requestId;
    previewRequestIdsRef.current.set(tabId, requestId);
    dispatch(existing ? { type: "replace-tab", tabId, tab: loadingTab } : { type: "open-tab", tab: loadingTab });
    try {
      const file = await readDesktopFilePreview(normalizedPath, {
        workspaceRoot: activeWorkspaceRoot,
      });
      if (previewRequestIdsRef.current.get(tabId) !== requestId) return;
      dispatch({
        type: "replace-tab",
        tabId,
        tab: {
          ...loadingTab,
          title: file.name || loadingTab.title,
          path: file.path,
          content: file.content,
          contentKind: file.contentKind,
          mimeType: file.mimeType,
          dataUrl: file.dataUrl,
          byteLen: file.byteLen,
          loading: false,
        },
      });
    } catch (error) {
      if (previewRequestIdsRef.current.get(tabId) !== requestId) return;
      dispatch({
        type: "replace-tab",
        tabId,
        tab: {
          ...loadingTab,
          loading: false,
          error:
            error instanceof Error && error.message.trim()
              ? error.message
              : typeof error === "string" && error.trim()
                ? error
                : t("useWorkspacePanelController.unableToReadFile"),
        },
      });
    } finally {
      if (previewRequestIdsRef.current.get(tabId) === requestId) {
        previewRequestIdsRef.current.delete(tabId);
      }
    }
  }, [dispatch, getPanelState]);

  const refreshActiveFile = useCallback(async () => {
    const current = getPanelState();
    const tab = current.tabs.find((entry) => entry.id === current.activeTabId);
    if (tab?.kind === "file" && tab.path && !previewRequestIdsRef.current.has(tab.id)) {
      await openFilePath(tab.path, { refresh: true, startLine: tab.targetLine, endLine: tab.targetEndLine });
    }
  }, [openFilePath, getPanelState]);

  useEffect(() => {
    if (typeof window === "undefined") return;
    const refresh = () => {
      if (getPanelState().isOpen && document.visibilityState === "visible") void refreshActiveFile();
    };
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    return () => { window.removeEventListener("focus", refresh); document.removeEventListener("visibilitychange", refresh); };
  }, [refreshActiveFile, getPanelState]);

  const openFile = useCallback(
    (entry: WorkspaceFileTreeEntry) => {
      if (!entry.isDirectory) void openFilePath(entry.path);
    },
    [openFilePath],
  );

  const openAgentSession = useCallback((sessionId: string, title: string) => {
    const normalizedSessionId = sessionId.trim();
    if (!normalizedSessionId) return;
    const tabId = `agent:${normalizedSessionId}`;
    const { sessions: currentSessions, currentSessionId: activeSessionId } = inputsRef.current;
    const child = currentSessions.find((session) => session.id === normalizedSessionId);
    const currentSession = currentSessions.find((session) => session.id === activeSessionId);
    const parentSessionId = child?.parentSessionId || currentSession?.id;
    const parent = currentSessions.find((session) => session.id === parentSessionId);
    dispatch({
      type: "focus-or-open-tab",
      tab: {
        id: tabId,
        kind: "agent",
        title: title.trim() || child?.title || "Agent",
        sessionId: normalizedSessionId,
        parentSessionId,
        parentTitle:
          parent?.title ||
          currentSession?.title ||
          t("useWorkspacePanelController.mainConversation"),
      },
    });
  }, [dispatch]);

  const openWorkspaceView = useCallback((kind: "files" | "review", fromNewTabId?: string) => {
    const root = inputsRef.current.workspaceRoot;
    if (!root) return;
    if (kind === "files") {
      const current = getPanelState();
      const file =
        current.tabs.find(tab => tab.kind === "file" && tab.id === current.activeTabId) ??
        current.tabs.find(tab => tab.kind === "file" && tab.isPreview) ??
        current.tabs.find(tab => tab.kind === "file");
      if (file) {
        dispatch({ type: "focus-or-open-tab", tab: file, fromNewTabId });
        return;
      }
    }
    dispatch({
      type: "focus-or-open-tab",
      tab: {
        id: `${kind}:${normalizeRoot(root)}`,
        kind,
        title: kind === "files" ? "Files" : "Review",
        workspaceRoot: root,
      },
      fromNewTabId,
    });
  }, [dispatch, getPanelState]);

  const openTasks = useCallback(() => {
    const { currentSessionId } = inputsRef.current;
    if (!currentSessionId) return;
    dispatch({ type: "focus-or-open-tab", tab: {
      id: `tasks:${currentSessionId}`, kind: "tasks", sessionId: currentSessionId,
      title: "Task",
    }});
  }, [dispatch]);

  const openNewTab = useCallback(() => {
    const { workspaceRoot, currentSessionId } = inputsRef.current;
    dispatch({ type: "focus-or-open-tab", tab: { id: `new:${crypto.randomUUID()}`, kind: "new", title: "New tab", workspaceRoot: workspaceRoot ?? undefined, sessionId: currentSessionId ?? undefined } });
  }, [dispatch]);

  const openTerminal = useCallback((terminal:TerminalSnapshot,serviceInstanceId:string,title?:string,fromNewTabId?:string)=>{
    dispatch({type:"focus-or-open-tab",fromNewTabId,tab:{id:`terminal:${terminal.terminalId}`,kind:"terminal",title:title || terminal.shell.split(/[\\/]/).pop() || "Terminal",workspaceRoot:terminal.workspaceRoot,terminalId:terminal.terminalId,serviceInstanceId}});
  },[dispatch]);
  const actions = useMemo(
    () => ({
      clear,
      saveScope,
      restoreScope,
      closeTab,
      collapse,
      openAgentSession,
      openFile,
      openFilePath,
      refreshActiveFile,
      openWorkspaceView,
      openTasks,
      openTerminal,
      openNewTab,
      removeAgentSessions,
      selectTab,
      show,
    }),
    [
      clear,
      saveScope,
      restoreScope,
      closeTab,
      collapse,
      openAgentSession,
      openFile,
      openFilePath,
      refreshActiveFile,
      openWorkspaceView,
      openTasks,
      openTerminal,
      openNewTab,
      removeAgentSessions,
      selectTab,
      show,
    ],
  );

  return {
    tabs: state.tabs,
    activeTabId: state.activeTabId,
    isOpen: state.isOpen,
    isVisible: state.isOpen && state.tabs.length > 0,
    actions,
  };
}
