import type { TerminalSnapshot } from "../../lib/terminalBridge";
import { t } from "../../i18n";
import { useCallback, useEffect, useMemo, useReducer, useRef } from "react";
import type { UiSession } from "../../types/ui";
import { readDesktopFilePreview, type WorkspaceFileTreeEntry } from "../../lib/workspaceBridge";
import type { SummaryPanelTab } from "../SummaryPanel";

type OpenWorkspacePathOptions = {
  refresh?: boolean;
  startLine?: number;
  endLine?: number;
  taskId?: string;
};

type PanelState = {
  tabs: SummaryPanelTab[];
  activeTabId: string | null;
  isOpen: boolean;
};

type PanelAction =
  | { type: "restore"; state: PanelState }
  | { type: "clear" }
  | { type: "collapse" }
  | { type: "show" }
  | { type: "select"; tabId: string }
  | { type: "browse-file"; tabId: string }
  | { type: "open-tab"; tab: SummaryPanelTab }
  | { type: "focus-or-open-tab"; tab: SummaryPanelTab }
  | { type: "replace-tab"; tabId: string; tab: SummaryPanelTab }
  | { type: "close-tab"; tabId: string }
  | { type: "remove-agent-sessions"; sessionIds: ReadonlySet<string> };

const initialPanelState: PanelState = {
  tabs: [],
  activeTabId: null,
  isOpen: true,
};

const normalizeRoot = (root?: string | null): string =>
  String(root || "")
    .trim()
    .replace(/^\\\\\?\\/, "")
    .replace(/\\/g, "/")
    .replace(/\/+$/, "")
    .toLowerCase();

const fileName = (path: string): string => path.split(/[\\/]/).filter(Boolean).at(-1) || path;

const removeTabs = (
  state: PanelState,
  shouldRemove: (tab: SummaryPanelTab) => boolean,
): PanelState => {
  const removedIndex = state.tabs.findIndex(
    (tab) => tab.id === state.activeTabId && shouldRemove(tab),
  );
  const tabs = state.tabs.filter((tab) => !shouldRemove(tab));
  const activeTabId =
    removedIndex >= 0
      ? (tabs[Math.min(removedIndex, Math.max(tabs.length - 1, 0))]?.id ?? null)
      : state.activeTabId && tabs.some((tab) => tab.id === state.activeTabId)
        ? state.activeTabId
        : (tabs[0]?.id ?? null);
  return { ...state, tabs, activeTabId };
};

// Three retained files plus one replaceable preview. Non-file views do not
// consume these slots. Reopening a retained file swaps it with the preview.
const placeFileTab = (current: SummaryPanelTab[], incoming: SummaryPanelTab): SummaryPanelTab[] => {
  const tabs = current.filter((tab) => tab.kind !== "files");
  const existingIndex = tabs.findIndex((tab) => tab.id === incoming.id);
  const previewIndex = tabs.findIndex((tab) => tab.kind === "file" && tab.isPreview);
  if (existingIndex >= 0) {
    tabs[existingIndex] = { ...incoming, isPreview: tabs[existingIndex].isPreview };
    if (previewIndex >= 0 && previewIndex !== existingIndex) {
      const previous = tabs[previewIndex];
      tabs[previewIndex] = { ...tabs[existingIndex], isPreview: true };
      tabs[existingIndex] = { ...previous, isPreview: false };
    }
    return tabs;
  }
  const retainedCount = tabs.filter((tab) => tab.kind === "file" && !tab.isPreview).length;
  const next = { ...incoming, isPreview: retainedCount >= 3 };
  if (previewIndex >= 0) {
    if (next.isPreview) tabs[previewIndex] = next;
    else tabs.splice(previewIndex, 0, next);
  } else {
    tabs.push(next);
  }
  return tabs;
};

const panelReducer = (state: PanelState, action: PanelAction): PanelState => {
  switch (action.type) {
    case "restore":
      return action.state;
    case "clear":
      return { ...state, tabs: [], activeTabId: null };
    case "collapse":
      return { ...state, isOpen: false };
    case "show":
      return { ...state, isOpen: true };
    case "select":
      return state.tabs.some((tab) => tab.id === action.tabId)
        ? { ...state, activeTabId: action.tabId }
        : state;
    case "browse-file": {
      const existing = state.tabs.find((tab) => tab.id === action.tabId);
      return existing
        ? {
            tabs: placeFileTab(state.tabs, existing),
            activeTabId: existing.id,
            isOpen: true,
          }
        : state;
    }
    case "open-tab": {
      const tabs = placeFileTab(state.tabs, action.tab);
      return {
        tabs,
        activeTabId: action.tab.id,
        isOpen: true,
      };
    }
    case "focus-or-open-tab": {
      const existing = state.tabs.find((tab) => tab.id === action.tab.id);
      return {
        tabs: existing ? state.tabs : [...state.tabs, action.tab],
        activeTabId: action.tab.id,
        isOpen: true,
      };
    }
    case "replace-tab":
      return {
        ...state,
        tabs: state.tabs.map((tab) =>
          tab.id === action.tabId ? { ...action.tab, isPreview: tab.isPreview } : tab,
        ),
      };
    case "close-tab":
      return removeTabs(state, (tab) => tab.id === action.tabId);
    case "remove-agent-sessions":
      return removeTabs(
        state,
        (tab) =>
          (tab.kind === "agent" || tab.kind === "tasks") &&
          Boolean(
            (tab.sessionId && action.sessionIds.has(tab.sessionId)) ||
              (tab.parentSessionId && action.sessionIds.has(tab.parentSessionId)),
          ),
      );
  }
};

export function useWorkspacePanelController({
  workspaceRoot,
  sessions,
  currentSessionId,
}: {
  workspaceRoot: string | null;
  sessions: UiSession[];
  currentSessionId: string | null;
}) {
  const [state, dispatch] = useReducer(panelReducer, initialPanelState);
  const savedScopes = useRef(new Map<string, PanelState>());
  const stateRef = useRef(state);
  stateRef.current = state;
  const inputsRef = useRef({ workspaceRoot, sessions, currentSessionId });
  const previewRequestIdsRef = useRef(new Map<string, number>());
  const nextPreviewRequestIdRef = useRef(0);
  inputsRef.current = { workspaceRoot, sessions, currentSessionId };

  const clear = useCallback(() => {
    previewRequestIdsRef.current.clear();
    dispatch({ type: "clear" });
  }, []);

  const saveScope = useCallback((key: string) => { savedScopes.current.set(key, stateRef.current); }, []);
  const restoreScope = useCallback((key: string) => {
    previewRequestIdsRef.current.clear();
    const saved = savedScopes.current.get(key);
    // Pending reads belong to the departed view. Reopening retries rather than leaving an endless spinner.
    const restored = saved ? { ...saved, tabs: saved.tabs.map(tab => tab.loading ? { ...tab, loading: false, error: "Preview interrupted. Reopen this file to retry." } : tab) } : initialPanelState;
    dispatch({ type: "restore", state: restored });
  }, []);

  const collapse = useCallback(() => {
    dispatch({ type: "collapse" });
  }, []);

  const show = useCallback(() => {
    dispatch({ type: "show" });
  }, []);

  const selectTab = useCallback((tabId: string) => {
    dispatch({ type: "select", tabId });
  }, []);

  const closeTab = useCallback((tabId: string) => {
    previewRequestIdsRef.current.delete(tabId);
    dispatch({ type: "close-tab", tabId });
  }, []);

  const removeAgentSessions = useCallback((sessionIds: ReadonlySet<string>) => {
    dispatch({ type: "remove-agent-sessions", sessionIds });
  }, []);

  const openFilePath = useCallback(async (path: string, options?: OpenWorkspacePathOptions) => {
    const normalizedPath = path.trim();
    const activeWorkspaceRoot = inputsRef.current.workspaceRoot;
    if (!normalizedPath || !activeWorkspaceRoot) return;
    const tabId = `file:${normalizeRoot(activeWorkspaceRoot)}:${normalizedPath.replace(/\\/g, "/").toLowerCase()}`;
    const existing = stateRef.current.tabs.find((tab) => tab.id === tabId);
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
  }, []);

  const refreshActiveFile = useCallback(async () => {
    const current = stateRef.current;
    const tab = current.tabs.find((entry) => entry.id === current.activeTabId);
    if (tab?.kind === "file" && tab.path && !previewRequestIdsRef.current.has(tab.id)) {
      await openFilePath(tab.path, { refresh: true, startLine: tab.targetLine, endLine: tab.targetEndLine });
    }
  }, [openFilePath]);

  useEffect(() => {
    if (typeof window === "undefined") return;
    const refresh = () => {
      if (stateRef.current.isOpen && document.visibilityState === "visible") void refreshActiveFile();
    };
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    return () => { window.removeEventListener("focus", refresh); document.removeEventListener("visibilitychange", refresh); };
  }, [refreshActiveFile]);

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
  }, []);

  const openWorkspaceView = useCallback((kind: "files" | "review") => {
    const root = inputsRef.current.workspaceRoot;
    if (!root) return;
    if (kind === "files") {
      const file =
        stateRef.current.tabs.find(
          (tab) => tab.kind === "file" && tab.id === stateRef.current.activeTabId,
        ) ??
        stateRef.current.tabs.find((tab) => tab.kind === "file" && tab.isPreview) ??
        stateRef.current.tabs.find((tab) => tab.kind === "file");
      if (file) {
        dispatch({ type: "select", tabId: file.id });
        dispatch({ type: "show" });
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
    });
  }, []);

  const openTasks = useCallback(() => {
    const { currentSessionId } = inputsRef.current;
    if (!currentSessionId) return;
    dispatch({ type: "focus-or-open-tab", tab: {
      id: `tasks:${currentSessionId}`, kind: "tasks", sessionId: currentSessionId,
      title: "Task",
    }});
  }, []);

  const openTerminal = useCallback((terminal:TerminalSnapshot,serviceInstanceId:string,title?:string)=>{
    dispatch({type:"focus-or-open-tab",tab:{id:`terminal:${terminal.terminalId}`,kind:"terminal",title:title || terminal.shell.split(/[\\/]/).pop() || "Terminal",workspaceRoot:terminal.workspaceRoot,terminalId:terminal.terminalId,serviceInstanceId}});
  },[]);
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
