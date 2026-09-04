import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { PanelLeft, PanelRight, X } from "lucide-react";
import { Sidebar, type ResourceModalKind } from "./components/Sidebar";
import { ChatArea } from "./components/chat/ChatArea";
import { sessionViewCacheStore } from "./components/chat/chatRuntimeCore";
import { PluginsDialog } from "./components/PluginsDialog";
import { ModelsDialog } from "./components/ModelsDialog";
import { SkillsDialog } from "./components/SkillsDialog";
import {
  ConfirmDialog,
  type ConfirmationRequest,
  type ConfirmAction,
} from "./components/ConfirmDialog";
import { SummaryPanel, type SummaryPanelTab } from "./components/SummaryPanel";
import type { UiSession } from "./types/ui";
import {
  activateSession,
  deleteSession,
  getAgentRuntimeConfig,
  listenAgentRuntimeConfigChanges,
  listSessions,
  updateSession,
  type SessionItem,
} from "./lib/chatBridge";
import {
  activateWorkspaceRoot,
  getWorkspaceGitHubCliStatus,
  getWorkspaceGitStatus,
  getWorkspaceInfo,
  openWorkspaceFolder,
  readDesktopFilePreview,
  resetWorkspaceCatalog,
  type WorkspaceOpenMode,
  type WorkspaceFileTreeEntry,
  type WorkspaceGitHubCliStatusResponse,
  type WorkspaceGitStatusResponse,
  type WorkspaceSnapshot,
} from "./lib/workspaceBridge";

type AppModal = ResourceModalKind | null;
type WorkspaceCatalogErrorState = { message: string; canReset: boolean };

const normalizeRoot = (root?: string | null): string =>
  String(root || "")
    .trim()
    .replace(/^\\\\\?\\/, "")
    .replace(/\\/g, "/")
    .replace(/\/+$/, "")
    .toLowerCase();

const fileName = (path: string): string =>
  path.split(/[\\/]/).filter(Boolean).at(-1) || path;

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

function App() {
  const [sessions, setSessions] = useState<UiSession[]>([]);
  const [currentSessionId, setCurrentSessionId] = useState<string | null>(null);
  const [workspaceSnapshot, setWorkspaceSnapshot] = useState<WorkspaceSnapshot>({
    activeWorkspaceRoot: null,
    workspaces: [],
    cancelled: false,
  });
  const [runningSessionIds, setRunningSessionIds] = useState<Set<string>>(new Set());
  const [completedSessionIds, setCompletedSessionIds] = useState<Set<string>>(new Set());
  const [gitStatus, setGitStatus] = useState<WorkspaceGitStatusResponse | null>(null);
  const [gitStatusError, setGitStatusError] = useState("");
  const [githubCliStatus, setGithubCliStatus] = useState<WorkspaceGitHubCliStatusResponse | null>(null);
  const [panelTabs, setPanelTabs] = useState<SummaryPanelTab[]>([]);
  const [activePanelTabId, setActivePanelTabId] = useState<string | null>(null);
  const [isSidebarOpen, setIsSidebarOpen] = useState(true);
  const [isFilePaneOpen, setIsFilePaneOpen] = useState(true);
  const [activeModal, setActiveModal] = useState<AppModal>(null);
  const [hasSelectableModel, setHasSelectableModel] = useState<boolean | null>(null);
  const [runtimeConfigRevision, setRuntimeConfigRevision] = useState(0);
  const [hostError, setHostError] = useState("");
  const [workspaceCatalogError, setWorkspaceCatalogError] = useState<WorkspaceCatalogErrorState | null>(null);
  const [confirmation, setConfirmation] = useState<ConfirmationRequest | null>(null);
  const currentSessionIdRef = useRef<string | null>(null);
  const confirmationResolverRef = useRef<((confirmed: boolean) => void) | null>(null);

  const workspaces = workspaceSnapshot.workspaces ?? [];
  const activeWorkspaceRoot = workspaceSnapshot.activeWorkspaceRoot ?? null;
  const currentSession = useMemo(
    () => sessions.find((session) => session.id === currentSessionId) ?? null,
    [currentSessionId, sessions],
  );
  const mainSessions = useMemo(
    () => sessions.filter((session) => session.sessionKind === "main"),
    [sessions],
  );
  const activeWorkspace = useMemo(
    () => workspaces.find((workspace) => normalizeRoot(workspace.root) === normalizeRoot(activeWorkspaceRoot)) ?? null,
    [activeWorkspaceRoot, workspaces],
  );

  const setCurrentSession = useCallback((sessionId: string | null) => {
    currentSessionIdRef.current = sessionId;
    setCurrentSessionId(sessionId);
  }, []);

  const confirmAction: ConfirmAction = useCallback((request) => {
    if (confirmationResolverRef.current) {
      return Promise.reject(new Error("A confirmation dialog is already open"));
    }
    setConfirmation(request);
    return new Promise<boolean>((resolve) => {
      confirmationResolverRef.current = resolve;
    });
  }, []);

  const answerConfirmation = useCallback((confirmed: boolean) => {
    const resolve = confirmationResolverRef.current;
    if (!resolve) return;
    confirmationResolverRef.current = null;
    setConfirmation(null);
    resolve(confirmed);
  }, []);

  const refreshSessions = useCallback(async (preferredSessionId?: string | null) => {
    const fetched = sortSessions((await listSessions()).map(toUiSession));
    setSessions(fetched);
    const preferred = preferredSessionId
      ? fetched.find(
        (session) => session.id === preferredSessionId && session.sessionKind === "main",
      ) ?? null
      : null;
    const workspaceMatch = fetched.find(
      (session) =>
        session.sessionKind === "main" &&
        normalizeRoot(session.cwd) === normalizeRoot(activeWorkspaceRoot),
    );
    const next = preferred ?? workspaceMatch ?? null;
    setCurrentSession(next?.id ?? null);
  }, [activeWorkspaceRoot, setCurrentSession]);

  useEffect(() => {
    let cancelled = false;
    void Promise.allSettled([
      getWorkspaceInfo(),
      listSessions(),
      getAgentRuntimeConfig(),
    ]).then(([workspaceResult, sessionResult, configResult]) => {
      if (cancelled) return;
      let snapshot: WorkspaceSnapshot = {
        activeWorkspaceRoot: null,
        workspaces: [],
        cancelled: false,
      };
      if (workspaceResult.status === "fulfilled") {
        snapshot = workspaceResult.value;
        setWorkspaceSnapshot(snapshot);
        setWorkspaceCatalogError(null);
      } else {
        const message = errorMessage(workspaceResult.reason, "加载工作区列表失败");
        setWorkspaceCatalogError({
          message,
          canReset: message.includes("workspace_catalog_corrupt"),
        });
      }
      if (sessionResult.status === "fulfilled") {
        const mapped = sortSessions(sessionResult.value.map(toUiSession));
        setSessions(mapped);
        const preferredId = snapshot.workspaces.find(
          (workspace) => normalizeRoot(workspace.root) === normalizeRoot(snapshot.activeWorkspaceRoot),
        )?.activeSessionId;
        const preferred = mapped.find(
          (session) => session.id === preferredId && session.sessionKind === "main",
        );
        setCurrentSession(preferred?.id ?? null);
      } else {
        setHostError(errorMessage(sessionResult.reason, "加载会话失败"));
      }
      if (configResult.status === "fulfilled") {
        setHasSelectableModel(configResult.value.selectableModels.length > 0);
      } else {
        setHasSelectableModel(false);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [setCurrentSession]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    void listenAgentRuntimeConfigChanges(() => {
      if (disposed) return;
      setRuntimeConfigRevision((revision) => revision + 1);
      void getAgentRuntimeConfig().then((config) => {
        if (!disposed) setHasSelectableModel(config.selectableModels.length > 0);
      }).catch((error) => {
        if (!disposed) setHostError(errorMessage(error, "加载模型配置失败"));
      });
    }).then((nextUnlisten) => {
      if (disposed) {
        nextUnlisten();
      } else {
        unlisten = nextUnlisten;
      }
    }).catch((error) => {
      if (!disposed) setHostError(errorMessage(error, "订阅模型配置失败"));
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    if (!activeWorkspaceRoot) {
      setGitStatus(null);
      setGithubCliStatus(null);
      setGitStatusError("");
      return;
    }
    let cancelled = false;
    void Promise.allSettled([
      getWorkspaceGitStatus(activeWorkspaceRoot),
      getWorkspaceGitHubCliStatus(),
    ]).then(([gitResult, githubResult]) => {
      if (cancelled) return;
      if (gitResult.status === "fulfilled") {
        setGitStatus(gitResult.value);
        setGitStatusError("");
      } else {
        setGitStatus(null);
        setGitStatusError(errorMessage(gitResult.reason, "Git 状态不可用"));
      }
      setGithubCliStatus(githubResult.status === "fulfilled" ? githubResult.value : null);
    });
    return () => {
      cancelled = true;
    };
  }, [activeWorkspaceRoot]);

  const handleWorkspaceCatalogFailure = (error: unknown, fallback: string) => {
    const message = errorMessage(error, fallback);
    if (message.includes("workspace_catalog_")) {
      setWorkspaceCatalogError({
        message,
        canReset: message.includes("workspace_catalog_corrupt"),
      });
      return;
    }
    setHostError(message);
  };

  const handleRetryWorkspaceCatalog = async () => {
    try {
      const snapshot = await getWorkspaceInfo();
      setWorkspaceSnapshot(snapshot);
      setWorkspaceCatalogError(null);
    } catch (error) {
      handleWorkspaceCatalogFailure(error, "重新加载工作区列表失败");
    }
  };

  const handleResetWorkspaceCatalog = async () => {
    try {
      const confirmed = await confirmAction({
        title: "Reset the workspace list?",
        message: "This removes the saved workspace list. Project files stay unchanged.",
      });
      if (!confirmed) return;
      const response = await resetWorkspaceCatalog();
      setWorkspaceSnapshot(response.snapshot);
      setWorkspaceCatalogError(null);
      setCurrentSession(null);
      setPanelTabs([]);
      setActivePanelTabId(null);
    } catch (error) {
      handleWorkspaceCatalogFailure(error, "重置工作区列表失败");
    }
  };

  const handleOpenWorkspace = async (mode: WorkspaceOpenMode) => {
    try {
      const snapshot = await openWorkspaceFolder(mode);
      if (snapshot.cancelled) return;
      setWorkspaceSnapshot(snapshot);
      setCurrentSession(null);
      setPanelTabs([]);
      setActivePanelTabId(null);
    } catch (error) {
      handleWorkspaceCatalogFailure(error, "打开工作区失败");
    }
  };

  const handleSelectWorkspace = async (root: string) => {
    try {
      const snapshot = await activateWorkspaceRoot(root);
      setWorkspaceSnapshot(snapshot);
      setCurrentSession(null);
      setPanelTabs([]);
      setActivePanelTabId(null);
    } catch (error) {
      handleWorkspaceCatalogFailure(error, "切换工作区失败");
    }
  };

  const handleSelectSession = async (sessionId: string) => {
    let target = sessions.find((session) => session.id === sessionId);
    if (!target) {
      const fetched = sortSessions((await listSessions()).map(toUiSession));
      setSessions(fetched);
      target = fetched.find((session) => session.id === sessionId);
    }
    if (!target) return;
    if (target.sessionKind !== "main") return;
    if (target.cwd && normalizeRoot(target.cwd) !== normalizeRoot(activeWorkspaceRoot)) {
      try {
        const snapshot = await activateWorkspaceRoot(target.cwd);
        setWorkspaceSnapshot(snapshot);
        setPanelTabs([]);
        setActivePanelTabId(null);
      } catch (error) {
        setHostError(errorMessage(error, "切换会话工作区失败"));
        return;
      }
    }
    setCurrentSession(sessionId);
    setCompletedSessionIds((previous) => {
      const next = new Set(previous);
      next.delete(sessionId);
      return next;
    });
    if (target.isUnread) {
      setSessions((previous) => previous.map((session) => session.id === sessionId ? { ...session, isUnread: false } : session));
      void updateSession(sessionId, { isUnread: false }).catch(() => undefined);
    }
    if (target.sessionKind === "main") {
      void activateSession(sessionId, Date.now() * 1_000).catch(() => undefined);
    }
  };

  const handleRenameSession = async (sessionId: string, title: string) => {
    try {
      const updated = toUiSession(await updateSession(sessionId, { title }));
      setSessions((items) => sortSessions(items.map((session) => session.id === sessionId ? updated : session)));
    } catch (error) {
      setHostError(errorMessage(error, "重命名会话失败"));
      throw error;
    }
  };

  const handleDeleteSession = async (sessionId: string) => {
    try {
      const target = sessions.find((session) => session.id === sessionId);
      if (!target) throw new Error(`删除目标会话不存在: ${sessionId}`);
      const response = await deleteSession(sessionId);
      if (response.deletedSessionId !== sessionId) {
        throw new Error(`删除会话响应身份不匹配: ${response.deletedSessionId}`);
      }
      const deletedIds = new Set(
        sessions
          .filter((session) => session.id === sessionId || session.parentSessionId === sessionId)
          .map((session) => session.id),
      );
      deletedIds.forEach((id) => sessionViewCacheStore.delete(id));
      setPanelTabs((tabs) => {
        const next = tabs.filter((tab) =>
          tab.kind !== "agent"
          || ((!tab.sessionId || !deletedIds.has(tab.sessionId))
            && tab.parentSessionId !== sessionId),
        );
        if (activePanelTabId && !next.some((tab) => tab.id === activePanelTabId)) {
          setActivePanelTabId(next.at(-1)?.id ?? null);
        }
        return next;
      });
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
      await refreshSessions(
        currentSessionIdRef.current && !deletedIds.has(currentSessionIdRef.current)
          ? currentSessionIdRef.current
          : null,
      );
    } catch (error) {
      setHostError(errorMessage(error, "删除会话失败"));
      throw error;
    }
  };

  const handleSessionResolved = useCallback((session: UiSession, options?: { activate?: boolean }) => {
    setSessions((items) => sortSessions([session, ...items.filter((item) => item.id !== session.id)]));
    if (options?.activate) setCurrentSession(session.id);
  }, [setCurrentSession]);

  const handleRunningChange = useCallback((sessionId: string, running: boolean) => {
    setRunningSessionIds((previous) => {
      const next = new Set(previous);
      if (running) next.add(sessionId);
      else next.delete(sessionId);
      return next;
    });
  }, []);

  const handleSessionCompleted = useCallback((sessionId: string) => {
    handleRunningChange(sessionId, false);
    if (currentSessionIdRef.current !== sessionId) {
      setCompletedSessionIds((previous) => new Set(previous).add(sessionId));
      setSessions((items) => items.map((session) => session.id === sessionId ? { ...session, isUnread: true } : session));
    }
    void refreshSessions(currentSessionIdRef.current).catch(() => undefined);
  }, [handleRunningChange, refreshSessions]);

  const openFilePath = useCallback(async (
    path: string,
    options?: { startLine?: number; endLine?: number; taskId?: string },
  ) => {
    const normalizedPath = path.trim();
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
    setPanelTabs((tabs) => tabs.some((tab) => tab.id === tabId) ? tabs.map((tab) => tab.id === tabId ? loadingTab : tab) : [...tabs, loadingTab]);
    setActivePanelTabId(tabId);
    setIsFilePaneOpen(true);
    try {
      const file = await readDesktopFilePreview(normalizedPath, { workspaceRoot: activeWorkspaceRoot });
      setPanelTabs((tabs) => tabs.map((tab) => tab.id === tabId ? {
        ...loadingTab,
        title: file.name || loadingTab.title,
        path: file.path,
        content: file.content,
        contentKind: file.contentKind,
        mimeType: file.mimeType,
        dataUrl: file.dataUrl,
        byteLen: file.byteLen,
        loading: false,
      } : tab));
    } catch (error) {
      setPanelTabs((tabs) => tabs.map((tab) => tab.id === tabId ? {
        ...loadingTab,
        loading: false,
        error: errorMessage(error, "读取文件失败"),
      } : tab));
    }
  }, [activeWorkspaceRoot]);

  const openAgentSession = useCallback((sessionId: string, title: string) => {
    const normalizedSessionId = sessionId.trim();
    if (!normalizedSessionId) return;
    const tabId = `agent:${normalizedSessionId}`;
    setPanelTabs((tabs) => {
      if (tabs.some((tab) => tab.id === tabId)) return tabs;
      const child = sessions.find((session) => session.id === normalizedSessionId);
      const parentSessionId = child?.parentSessionId || currentSession?.id;
      const parent = sessions.find((session) => session.id === parentSessionId);
      return [...tabs, {
        id: tabId,
        kind: "agent",
        title: title.trim() || child?.title || "Agent",
        sessionId: normalizedSessionId,
        parentSessionId,
        parentTitle: parent?.title || currentSession?.title || "主会话",
      }];
    });
    setActivePanelTabId(tabId);
    setIsFilePaneOpen(true);
  }, [currentSession, sessions]);

  const handleOpenFile = (entry: WorkspaceFileTreeEntry) => {
    if (!entry.isDirectory) void openFilePath(entry.path);
  };

  const closePanelTab = (tabId: string) => {
    setPanelTabs((tabs) => {
      const index = tabs.findIndex((tab) => tab.id === tabId);
      const next = tabs.filter((tab) => tab.id !== tabId);
      if (activePanelTabId === tabId) {
        setActivePanelTabId(next[Math.min(index, Math.max(next.length - 1, 0))]?.id ?? null);
      }
      return next;
    });
  };

  const modalTitle = activeModal === "models"
    ? "Models"
    : activeModal === "skills"
      ? "Skills"
      : "Plugins";

  const chatWorkspaceRoot = currentSession?.cwd ?? activeWorkspaceRoot;
  const isFilePaneVisible = isFilePaneOpen && panelTabs.length > 0;

  return (
    <div className={`thinAppShell ${isSidebarOpen ? "is-sidebar-open" : "is-sidebar-collapsed"}`}>
      <div className="thinSidebarBrand">
        <strong><img src="./centaeris-mark.png" alt="" />Centaeris</strong>
      </div>
      <header className="nativeTitlebar">
        <div className="nativeTitlebarSafeArea">
          <button
            type="button"
            className="nativePanelToggle"
            onClick={() => setIsSidebarOpen((open) => !open)}
            aria-label={isSidebarOpen ? "Hide left sidebar" : "Show left sidebar"}
            aria-expanded={isSidebarOpen}
            title={isSidebarOpen ? "Hide left sidebar" : "Show left sidebar"}
          >
            <PanelLeft aria-hidden="true" />
          </button>
          {!isFilePaneVisible && panelTabs.length > 0 ? (
            <button
              type="button"
              className="nativePanelToggle is-right"
              onClick={() => setIsFilePaneOpen(true)}
              aria-label="Show right sidebar"
              aria-expanded={false}
              title="Show right sidebar"
            >
              <PanelRight aria-hidden="true" />
            </button>
          ) : null}
        </div>
      </header>
      <div className="thinSidebarSlot">
        <Sidebar
          sessions={mainSessions}
          currentSessionId={currentSessionId}
          workspaces={workspaces}
          activeWorkspaceRoot={activeWorkspaceRoot}
          runningSessionIds={runningSessionIds}
          completedSessionIds={completedSessionIds}
          workspaceCatalogError={workspaceCatalogError}
          onNewChat={() => setCurrentSession(null)}
          onOpenWorkspace={(mode) => void handleOpenWorkspace(mode)}
          onSelectWorkspace={(root) => void handleSelectWorkspace(root)}
          onRetryWorkspaceCatalog={handleRetryWorkspaceCatalog}
          onResetWorkspaceCatalog={handleResetWorkspaceCatalog}
          onSelectSession={(sessionId) => void handleSelectSession(sessionId)}
          onRenameSession={handleRenameSession}
          onDeleteSession={handleDeleteSession}
          onOpenResource={setActiveModal}
          onOpenFile={handleOpenFile}
        />
      </div>
      <div className="thinWorkspaceShell">
        <div className="thinWorkspaceBody">
          <main className="thinChatColumn">
            {!activeWorkspaceRoot ? (
              <section className="thinGetStarted">
                <h1>Get Started</h1>
                <ol>
                  <li>Select a project directory from the sidebar</li>
                  <li>Add models via the <button type="button" onClick={() => setActiveModal("models")}>Models</button> button at the bottom</li>
                </ol>
              </section>
            ) : hasSelectableModel === false ? (
              <section className="thinGetStarted">
                <h1>Configure a model</h1>
                <p>Use <button type="button" onClick={() => setActiveModal("models")}>Models</button> in the sidebar to connect a provider.</p>
              </section>
            ) : (
              <ChatArea
                currentSession={currentSession}
                currentSessionId={currentSessionId}
                workspaceName={activeWorkspace?.name ?? "Workspace"}
                workspaceRoot={chatWorkspaceRoot}
                gitStatus={gitStatus}
                gitStatusError={gitStatusError}
                githubCliStatus={githubCliStatus}
                runtimeConfigRevision={runtimeConfigRevision}
                onOpenWorkspacePath={openFilePath}
                onOpenAgentSession={openAgentSession}
                onNewSession={() => setCurrentSession(null)}
                onOpenResource={setActiveModal}
                onSessionResolved={handleSessionResolved}
                onAgentRunningChange={handleRunningChange}
                onSessionCompleted={handleSessionCompleted}
              />
            )}
          </main>

          <aside className={`thinFilePane ${isFilePaneVisible ? "is-open" : ""}`} aria-label="Preview" aria-hidden={!isFilePaneVisible}>
            {panelTabs.length > 0 ? (
              <SummaryPanel
                tabs={panelTabs}
                activeTabId={activePanelTabId}
                onSelectTab={setActivePanelTabId}
                onCloseTab={closePanelTab}
                onCollapse={() => setIsFilePaneOpen(false)}
                onOpenWorkspacePath={openFilePath}
              />
            ) : null}
          </aside>
        </div>
      </div>

      {activeModal ? (
        <div className="resourceDialogOverlay" role="presentation" onMouseDown={() => setActiveModal(null)}>
          <section className="resourceDialog" role="dialog" aria-modal="true" aria-label={modalTitle} onMouseDown={(event) => event.stopPropagation()}>
            <header className="resourceDialogHeader">
              <h1>
                <span>{modalTitle}</span>
              </h1>
              <button type="button" onClick={() => setActiveModal(null)} aria-label="关闭"><X aria-hidden="true" /></button>
            </header>
            <div className="resourceDialogBody">
              {activeModal === "models" ? (
                <ModelsDialog
                  onClose={() => setActiveModal(null)}
                  onConfigured={setHasSelectableModel}
                  confirmAction={confirmAction}
                />
              ) : activeModal === "skills" ? (
                <SkillsDialog workspaceRoot={activeWorkspaceRoot} confirmAction={confirmAction} />
              ) : activeModal === "plugins" ? (
                <PluginsDialog />
              ) : null}
            </div>
          </section>
        </div>
      ) : null}

      <ConfirmDialog
        open={Boolean(confirmation)}
        title={confirmation?.title ?? ""}
        message={confirmation?.message}
        onCancel={() => answerConfirmation(false)}
        onConfirm={() => answerConfirmation(true)}
      />

      {hostError ? (
        <div className="thinHostError" role="alert"><span>{hostError}</span><button type="button" onClick={() => setHostError("")}><X aria-hidden="true" /></button></div>
      ) : null}
    </div>
  );
}

export default App;
