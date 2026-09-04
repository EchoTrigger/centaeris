import { useCallback, useMemo, useReducer, useRef } from "react";
import type { UiSession } from "../../types/ui";
import {
  readDesktopFilePreview,
  type WorkspaceFileTreeEntry,
} from "../../lib/workspaceBridge";
import type { SummaryPanelTab } from "../SummaryPanel";

type OpenWorkspacePathOptions = {
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
  | { type: "clear" }
  | { type: "collapse" }
  | { type: "show" }
  | { type: "select"; tabId: string }
  | { type: "open-tab"; tab: SummaryPanelTab }
  | { type: "open-agent-tab"; tab: SummaryPanelTab }
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

const fileName = (path: string): string =>
  path.split(/[\\/]/).filter(Boolean).at(-1) || path;

const removeTabs = (
  state: PanelState,
  shouldRemove: (tab: SummaryPanelTab) => boolean,
): PanelState => {
  const removedIndex = state.tabs.findIndex(
    (tab) => tab.id === state.activeTabId && shouldRemove(tab),
  );
  const tabs = state.tabs.filter((tab) => !shouldRemove(tab));
  const activeTabId = removedIndex >= 0
    ? tabs[Math.min(removedIndex, Math.max(tabs.length - 1, 0))]?.id ?? null
    : state.activeTabId && tabs.some((tab) => tab.id === state.activeTabId)
      ? state.activeTabId
      : tabs[0]?.id ?? null;
  return { ...state, tabs, activeTabId };
};

const panelReducer = (state: PanelState, action: PanelAction): PanelState => {
  switch (action.type) {
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
    case "open-tab": {
      const tabs = state.tabs.some((tab) => tab.id === action.tab.id)
        ? state.tabs.map((tab) => tab.id === action.tab.id ? action.tab : tab)
        : [...state.tabs, action.tab];
      return {
        tabs,
        activeTabId: action.tab.id,
        isOpen: true,
      };
    }
    case "open-agent-tab": {
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
        tabs: state.tabs.map((tab) => tab.id === action.tabId ? action.tab : tab),
      };
    case "close-tab":
      return removeTabs(state, (tab) => tab.id === action.tabId);
    case "remove-agent-sessions":
      return removeTabs(
        state,
        (tab) => tab.kind === "agent"
          && Boolean(
            (tab.sessionId && action.sessionIds.has(tab.sessionId))
            || (tab.parentSessionId && action.sessionIds.has(tab.parentSessionId)),
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
  const inputsRef = useRef({ workspaceRoot, sessions, currentSessionId });
  const previewRequestIdsRef = useRef(new Map<string, number>());
  const nextPreviewRequestIdRef = useRef(0);
  inputsRef.current = { workspaceRoot, sessions, currentSessionId };

  const clear = useCallback(() => {
    previewRequestIdsRef.current.clear();
    dispatch({ type: "clear" });
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

  const openFilePath = useCallback(async (
    path: string,
    options?: OpenWorkspacePathOptions,
  ) => {
    const normalizedPath = path.trim();
    const activeWorkspaceRoot = inputsRef.current.workspaceRoot;
    if (!normalizedPath || !activeWorkspaceRoot) return;
    const tabId = `file:${normalizeRoot(activeWorkspaceRoot)}:${normalizedPath.replace(/\\/g, "/").toLowerCase()}`;
    const loadingTab: SummaryPanelTab = {
      id: tabId,
      kind: "file",
      title: fileName(normalizedPath),
      path: normalizedPath,
      targetLine: options?.startLine,
      targetEndLine: options?.endLine,
      loading: true,
    };
    const requestId = nextPreviewRequestIdRef.current + 1;
    nextPreviewRequestIdRef.current = requestId;
    previewRequestIdsRef.current.set(tabId, requestId);
    dispatch({ type: "open-tab", tab: loadingTab });
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
          error: error instanceof Error && error.message.trim()
            ? error.message
            : typeof error === "string" && error.trim()
              ? error
              : "读取文件失败",
        },
      });
    }
  }, []);

  const openFile = useCallback((entry: WorkspaceFileTreeEntry) => {
    if (!entry.isDirectory) void openFilePath(entry.path);
  }, [openFilePath]);

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
      type: "open-agent-tab",
      tab: {
        id: tabId,
        kind: "agent",
        title: title.trim() || child?.title || "Agent",
        sessionId: normalizedSessionId,
        parentSessionId,
        parentTitle: parent?.title || currentSession?.title || "主会话",
      },
    });
  }, []);

  const actions = useMemo(() => ({
    clear,
    closeTab,
    collapse,
    openAgentSession,
    openFile,
    openFilePath,
    removeAgentSessions,
    selectTab,
    show,
  }), [
    clear,
    closeTab,
    collapse,
    openAgentSession,
    openFile,
    openFilePath,
    removeAgentSessions,
    selectTab,
    show,
  ]);

  return {
    tabs: state.tabs,
    activeTabId: state.activeTabId,
    isOpen: state.isOpen,
    isVisible: state.isOpen && state.tabs.length > 0,
    actions,
  };
}
