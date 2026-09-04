import { useCallback, useEffect, useRef, useState } from "react";
import { PanelLeft, PanelRight, X } from "lucide-react";
import { Sidebar, type ResourceModalKind } from "./components/Sidebar";
import { ChatArea } from "./components/chat/ChatArea";
import { PluginsDialog } from "./components/PluginsDialog";
import { ModelsDialog } from "./components/ModelsDialog";
import { SkillsDialog } from "./components/SkillsDialog";
import {
  ConfirmDialog,
  type ConfirmationRequest,
  type ConfirmAction,
} from "./components/ConfirmDialog";
import { SummaryPanel } from "./components/SummaryPanel";
import { useSessionController } from "./components/app/useSessionController";
import { useWorkspaceController } from "./components/app/useWorkspaceController";
import { useWorkspacePanelController } from "./components/app/useWorkspacePanelController";
import {
  getAgentRuntimeConfig,
  listenAgentRuntimeConfigChanges,
  listSessions,
} from "./lib/chatBridge";
import {
  getWorkspaceInfo,
  type WorkspaceSnapshot,
} from "./lib/workspaceBridge";

type AppModal = ResourceModalKind | null;

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

function App() {
  const [isSidebarOpen, setIsSidebarOpen] = useState(true);
  const [activeModal, setActiveModal] = useState<AppModal>(null);
  const [hasSelectableModel, setHasSelectableModel] = useState<boolean | null>(null);
  const [runtimeConfigRevision, setRuntimeConfigRevision] = useState(0);
  const [hostError, setHostError] = useState("");
  const [confirmation, setConfirmation] = useState<ConfirmationRequest | null>(null);
  const confirmationResolverRef = useRef<((confirmed: boolean) => void) | null>(null);
  const runtimeConfigRequestIdRef = useRef(0);

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

  const workspaceController = useWorkspaceController({
    confirmAction,
    reportHostError: setHostError,
  });
  const {
    workspaces,
    activeWorkspaceRoot,
    activeWorkspace,
    catalogError: workspaceCatalogError,
    gitStatus,
    gitStatusError,
    githubCliStatus,
  } = workspaceController;
  const {
    applySnapshot: applyWorkspaceSnapshot,
    beginInitialization: beginWorkspaceInitialization,
    openWorkspace,
    reportCatalogFailure: reportWorkspaceCatalogFailure,
    resetCatalog,
    retryCatalog,
    selectWorkspace,
  } = workspaceController.actions;
  const sessionController = useSessionController({
    activeWorkspaceRoot,
    reportError: setHostError,
  });
  const {
    sessions,
    currentSessionId,
    currentSession,
    mainSessions,
    runningSessionIds,
    completedSessionIds,
  } = sessionController;
  const {
    beginInitialization: beginSessionInitialization,
    clearSelection,
    completeSession,
    removeSession,
    renameSession,
    resolveSession,
    selectSession,
    setRunning,
  } = sessionController.actions;
  const workspacePanel = useWorkspacePanelController({
    workspaceRoot: activeWorkspaceRoot,
    sessions,
    currentSessionId,
  });
  const {
    clear: clearWorkspacePanel,
    removeAgentSessions,
  } = workspacePanel.actions;

  useEffect(() => {
    let cancelled = false;
    const workspaceInitialization = beginWorkspaceInitialization();
    const sessionInitialization = beginSessionInitialization();
    const runtimeConfigRequestId = runtimeConfigRequestIdRef.current + 1;
    runtimeConfigRequestIdRef.current = runtimeConfigRequestId;
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
        workspaceInitialization.applySnapshot(snapshot);
      } else if (workspaceInitialization.isCurrent()) {
        reportWorkspaceCatalogFailure(workspaceResult.reason, "加载工作区列表失败");
      }
      if (sessionResult.status === "fulfilled") {
        const preferredId = snapshot.workspaces.find(
          (workspace) => normalizeRoot(workspace.root) === normalizeRoot(snapshot.activeWorkspaceRoot),
        )?.activeSessionId;
        sessionInitialization.applySessions(sessionResult.value, preferredId);
      } else if (sessionInitialization.isCurrent()) {
        setHostError(errorMessage(sessionResult.reason, "加载会话失败"));
      }
      if (runtimeConfigRequestIdRef.current === runtimeConfigRequestId) {
        if (configResult.status === "fulfilled") {
          setHasSelectableModel(configResult.value.selectableModels.length > 0);
        } else {
          setHasSelectableModel(false);
        }
      }
    });
    return () => {
      cancelled = true;
    };
  }, [
    beginSessionInitialization,
    beginWorkspaceInitialization,
    reportWorkspaceCatalogFailure,
  ]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    void listenAgentRuntimeConfigChanges(() => {
      if (disposed) return;
      const requestId = runtimeConfigRequestIdRef.current + 1;
      runtimeConfigRequestIdRef.current = requestId;
      setRuntimeConfigRevision((revision) => revision + 1);
      void getAgentRuntimeConfig().then((config) => {
        if (!disposed && runtimeConfigRequestIdRef.current === requestId) {
          setHasSelectableModel(config.selectableModels.length > 0);
        }
      }).catch((error) => {
        if (!disposed && runtimeConfigRequestIdRef.current === requestId) {
          setHostError(errorMessage(error, "加载模型配置失败"));
        }
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

  const handleResetWorkspaceCatalog = useCallback(async () => {
    if (!await resetCatalog()) return;
    clearSelection();
    clearWorkspacePanel();
  }, [clearSelection, clearWorkspacePanel, resetCatalog]);

  const handleOpenWorkspace = useCallback(async (mode: Parameters<typeof openWorkspace>[0]) => {
    if (!await openWorkspace(mode)) return;
    clearSelection();
    clearWorkspacePanel();
  }, [clearSelection, clearWorkspacePanel, openWorkspace]);

  const handleSelectWorkspace = useCallback(async (root: string) => {
    if (!await selectWorkspace(root)) return;
    clearSelection();
    clearWorkspacePanel();
  }, [clearSelection, clearWorkspacePanel, selectWorkspace]);

  const handleSelectSession = useCallback(async (sessionId: string) => {
    const result = await selectSession(sessionId);
    if (!result?.workspaceSnapshot) return;
    applyWorkspaceSnapshot(result.workspaceSnapshot);
    clearWorkspacePanel();
  }, [applyWorkspaceSnapshot, clearWorkspacePanel, selectSession]);

  const handleDeleteSession = useCallback(async (sessionId: string) => {
    const deletedIds = await removeSession(sessionId);
    removeAgentSessions(deletedIds);
  }, [removeAgentSessions, removeSession]);

  const modalTitle = activeModal === "models"
    ? "Models"
    : activeModal === "skills"
      ? "Skills"
      : "Plugins";

  const chatWorkspaceRoot = currentSession?.cwd ?? activeWorkspaceRoot;
  const isFilePaneVisible = workspacePanel.isVisible;

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
          {!isFilePaneVisible && workspacePanel.tabs.length > 0 ? (
            <button
              type="button"
              className="nativePanelToggle is-right"
              onClick={workspacePanel.actions.show}
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
          onNewChat={clearSelection}
          onOpenWorkspace={(mode) => void handleOpenWorkspace(mode)}
          onSelectWorkspace={(root) => void handleSelectWorkspace(root)}
          onRetryWorkspaceCatalog={retryCatalog}
          onResetWorkspaceCatalog={handleResetWorkspaceCatalog}
          onSelectSession={(sessionId) => void handleSelectSession(sessionId)}
          onRenameSession={renameSession}
          onDeleteSession={handleDeleteSession}
          onOpenResource={setActiveModal}
          onOpenFile={workspacePanel.actions.openFile}
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
                onOpenWorkspacePath={workspacePanel.actions.openFilePath}
                onOpenAgentSession={workspacePanel.actions.openAgentSession}
                onNewSession={clearSelection}
                onOpenResource={setActiveModal}
                onSessionResolved={resolveSession}
                onAgentRunningChange={setRunning}
                onSessionCompleted={completeSession}
              />
            )}
          </main>

          <aside className={`thinFilePane ${isFilePaneVisible ? "is-open" : ""}`} aria-label="Preview" aria-hidden={!isFilePaneVisible}>
            {workspacePanel.tabs.length > 0 ? (
              <SummaryPanel
                tabs={workspacePanel.tabs}
                activeTabId={workspacePanel.activeTabId}
                onSelectTab={workspacePanel.actions.selectTab}
                onCloseTab={workspacePanel.actions.closeTab}
                onCollapse={workspacePanel.actions.collapse}
                onOpenWorkspacePath={workspacePanel.actions.openFilePath}
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
