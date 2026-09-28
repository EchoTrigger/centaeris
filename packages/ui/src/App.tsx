import { querySessionCatalog } from "./lib/chatBridge";
import { ChatActionsMenu } from "./components/ChatActionsMenu";
import { SettingsPage } from "./components/SettingsPage";
import { ApplicationBar } from "./components/ApplicationBar";
import { SchedulesPage } from "./components/SchedulesPage";
import { useAppNavigation, type AppPage } from "./components/app/useAppNavigation";
import { listenHost } from "./host/hostBridge";
import { ThemeToggle } from "./components/ThemeToggle";
import { t } from "./i18n";
import { useCallback, useEffect, useRef, useState, type CSSProperties } from "react";
import { PanelRight, X } from "lucide-react";
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
import { WorkspaceOverview } from "./components/WorkspaceOverview";
import { WorkspaceSplitHandle } from "./components/WorkspaceSplitHandle";
import { SummaryPanel } from "./components/SummaryPanel";
import { useSessionController } from "./components/app/useSessionController";
import { useWorkspaceController } from "./components/app/useWorkspaceController";
import { useWorkspacePanelController } from "./components/app/useWorkspacePanelController";
import {
  getAgentRuntimeConfig,
  listenAgentRuntimeConfigChanges,
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
  const [workspacePanelRatio, setWorkspacePanelRatio] = useState(0.5);
  const [page, setPage] = useState<AppPage>("chat");
  const [pluginId, setPluginId] = useState("");
  const [resourceTab, setResourceTab] = useState<"plugins" | "skills">("plugins");
  const [workspaceBootstrapPending, setWorkspaceBootstrapPending] = useState(true);
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
    renameWorkspace,
    removeWorkspace,
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
    pinSession,
    reorderPinnedSessions,
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
    const workspaceRequest = getWorkspaceInfo().then(snapshot => {
      if (!cancelled) { workspaceInitialization.applySnapshot(snapshot); setWorkspaceBootstrapPending(false); }
      return snapshot;
    }, error => {
      if (!cancelled) {
        setWorkspaceBootstrapPending(false);
        if (workspaceInitialization.isCurrent()) reportWorkspaceCatalogFailure(error, t("app.unableToLoadWorkspaces"));
      }
      throw error;
    });
    void Promise.allSettled([
      workspaceRequest,
      sessionInitialization.load(),
    ]).then(async ([workspaceResult, sessionResult]) => {
      if (cancelled) return;
      let snapshot: WorkspaceSnapshot = {
        activeWorkspaceRoot: null,
        workspaces: [],
        cancelled: false,
      };
      if (workspaceResult.status === "fulfilled") {
        snapshot = workspaceResult.value;
      }
      if (sessionResult.status === "fulfilled") {
        const preferredId = snapshot.workspaces.find(
          (workspace) => normalizeRoot(workspace.root) === normalizeRoot(snapshot.activeWorkspaceRoot),
        )?.activeSessionId;
        const items = sessionResult.value;
        if (preferredId && !items.some(item => item.id === preferredId)) {
          try { items.push(...(await querySessionCatalog({mode:"lookup",sessionId:preferredId})).items); }
          catch (error) { if (!cancelled) setHostError(errorMessage(error, t("app.unableToLoadConversations"))); }
        }
        if (!cancelled) sessionInitialization.applySessions(items, preferredId);
      } else if (sessionInitialization.isCurrent()) {
        setHostError(errorMessage(sessionResult.reason, t("app.unableToLoadConversations")));
      }
    });
    void getAgentRuntimeConfig().then(config => {
      if (!cancelled && runtimeConfigRequestIdRef.current === runtimeConfigRequestId)
        setHasSelectableModel(config.selectableModels.length > 0);
    }, () => {
      if (!cancelled && runtimeConfigRequestIdRef.current === runtimeConfigRequestId)
        setHasSelectableModel(false);
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
          setHostError(errorMessage(error, t("app.unableToLoadModelConfiguration")));
        }
      });
    }).then((nextUnlisten) => {
      if (disposed) {
        nextUnlisten();
      } else {
        unlisten = nextUnlisten;
      }
    }).catch((error) => {
      if (!disposed) setHostError(errorMessage(error, t("app.unableToSubscribeToModelConfiguration")));
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

  const navigation = useAppNavigation({ page, pluginId, resourceTab, sessionId: currentSessionId, root: activeWorkspaceRoot }, async (location, isCurrent) => {
    if (location.page === "chat") {
      const scope = (session: string | null, root: string | null) => session ? `session:${session}` : `draft:${root ?? ""}`;
      workspacePanel.actions.saveScope(scope(currentSessionId, activeWorkspaceRoot));
      if (location.sessionId && location.sessionId !== currentSessionId) {
        const result = await selectSession(location.sessionId);
        if (!result || !isCurrent()) return false;
        if (result.workspaceSnapshot) applyWorkspaceSnapshot(result.workspaceSnapshot);
        workspacePanel.actions.restoreScope(scope(location.sessionId, location.root));
      } else if (!location.sessionId) {
        if (location.root && location.root !== activeWorkspaceRoot && !await selectWorkspace(location.root)) return false;
        if (!isCurrent()) return false;
        clearSelection();
        workspacePanel.actions.restoreScope(scope(null, location.root));
      }
    }
    setPluginId(location.pluginId ?? "");
    if (location.resourceTab) setResourceTab(location.resourceTab);
    setPage(location.page);
    return true;
  });
  const navigatePage = (next: AppPage) => { void navigation.move({ page: next, sessionId: currentSessionId, root: activeWorkspaceRoot }); };
  const setActiveModal = (kind: AppModal) => {
    if (!kind) { navigatePage("chat"); return; }
    if (kind === "models") navigatePage("settings");
    else { setResourceTab(kind); navigatePage("plugins"); }
  };
  const handleOpenWorkspace = async (mode: Parameters<typeof openWorkspace>[0]) => {
    const origin = { page, pluginId, resourceTab, sessionId: currentSessionId, root: activeWorkspaceRoot };
    const snapshot = await openWorkspace(mode);
    if (!snapshot) return;
    navigation.accept({ page: "chat", sessionId: null, root: snapshot.activeWorkspaceRoot ?? null }, origin);
    clearSelection(); clearWorkspacePanel(); setPage("chat");
  };
  const handleSelectWorkspace = async (root: string) => { await navigation.move({ page: "chat", sessionId: null, root }); };
  const handleSelectSession = async (sessionId: string) => { await navigation.move({ page: "chat", sessionId, root: sessions.find(s => s.id === sessionId)?.cwd ?? activeWorkspaceRoot }); };
  const newChat = () => { void navigation.move({ page: "chat", sessionId: null, root: activeWorkspaceRoot }); };
  const menuActions = useRef<(action: string) => void>(() => {});
  menuActions.current = action => {
    if (action === "open-folder") void handleOpenWorkspace("customPath").catch(error => setHostError(String(error)));
    if (action === "new-chat") newChat();
    if (action === "settings" || action === "about") navigatePage(action);
    if (action === "toggle-sidebar") setIsSidebarOpen(value => !value);
    if (action === "toggle-panel") { if (workspacePanel.isVisible) workspacePanel.actions.collapse(); else workspacePanel.actions.show(); }
  };
  useEffect(() => {
    let disposed = false; let unlisten: (() => void) | undefined;
    void listenHost<{ action: string }>("centaeris/navigation", payload => menuActions.current(payload.action)).then(stop => { if (disposed) stop(); else unlisten = stop; }).catch(error => setHostError(String(error)));
    const keydown = (event: KeyboardEvent) => {
      if (event.target instanceof Element && event.target.closest(".workspaceTerminal")) return;
      if ((!event.ctrlKey && !event.metaKey) || event.altKey || event.shiftKey) return;
      const action = ({ o: "open-folder", n: "new-chat", b: "toggle-sidebar", ",": "settings" } as Record<string, string>)[event.key.toLowerCase()];
      if (action) { event.preventDefault(); menuActions.current(action); }
    };
    if (typeof window !== "undefined") window.addEventListener("keydown", keydown);
    return () => { disposed = true; unlisten?.(); if (typeof window !== "undefined") window.removeEventListener("keydown", keydown); };
  }, []);

  const handleDeleteSession = useCallback(async (sessionId: string) => {
    const deletedIds = await removeSession(sessionId);
    removeAgentSessions(deletedIds);
  }, [removeAgentSessions, removeSession]);

  const chatWorkspaceRoot = currentSession?.cwd ?? activeWorkspaceRoot;
  const chatWorkspace = workspaces.find(
    (workspace) => normalizeRoot(workspace.root) === normalizeRoot(chatWorkspaceRoot),
  ) ?? null;
  const chatUsesActiveWorkspace =
    normalizeRoot(chatWorkspaceRoot) === normalizeRoot(activeWorkspaceRoot);
  const isFilePaneVisible = page === "chat" && workspacePanel.isVisible;

  return (
    <div className={`thinAppShell ${isSidebarOpen ? "is-sidebar-open" : "is-sidebar-collapsed"}`}>
      <ApplicationBar expanded={isSidebarOpen} onToggle={() => setIsSidebarOpen(open => !open)} canBack={navigation.canBack} canForward={navigation.canForward} onBack={() => void navigation.move(-1)} onForward={() => void navigation.move(1)} onError={setHostError} />

      <div className="thinSidebarSlot">
        <Sidebar
          onExpandWorkspace={sessionController.actions.expandWorkspace}
          onLoadMore={sessionController.actions.loadMore}
          hasMore={sessionController.hasMore}
          activePage={page}
          collapsed={!isSidebarOpen}
          onNavigate={navigatePage}
          onPinSession={pinSession}
          onReorderPinnedSessions={reorderPinnedSessions}
          sessions={mainSessions}
          currentSessionId={currentSessionId}
          workspaces={workspaces}
          activeWorkspaceRoot={activeWorkspaceRoot}
          runningSessionIds={runningSessionIds}
          completedSessionIds={completedSessionIds}
          workspaceCatalogError={workspaceCatalogError}
          onNewChat={newChat}
          onOpenWorkspace={(mode) => void handleOpenWorkspace(mode)}
          onRenameWorkspace={renameWorkspace}
          onRemoveWorkspace={removeWorkspace}
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
      <div className={`thinWorkspaceShell ${page === "chat" ? "is-chat-page" : ""}`}>
        {page !== "chat" ? <header className="workspacePageHeader"><strong>{page === "schedules" ? "Scheduled" : page === "plugins" ? "Plugins" : page === "settings" ? "Settings" : "About Centaeris"}</strong></header> : null}

        <div className="thinWorkspaceBody" hidden={page !== "chat"} style={{ "--workspace-panel-width": `${workspacePanelRatio * 100}%` } as CSSProperties}>
          <main className="thinChatColumn">
        <header className="workspacePageHeader">
          <strong>{page === "chat" ? currentSession?.title ?? "New chat" : page === "schedules" ? "Scheduled" : page === "plugins" ? "Plugins" : page === "settings" ? "Settings" : "About Centaeris"}</strong>
          {page === "chat" ? <div className="workspacePageActions">
            {currentSession ? <ChatActionsMenu><button onClick={() => void pinSession(currentSession.id, !currentSession.isPinned).catch(() => undefined)}>{currentSession.isPinned ? "Unpin chat" : "Pin chat"}</button><button onClick={() => { void confirmAction({ title: "Delete chat?", message: currentSession.title }).then(confirmed => { if (confirmed) void handleDeleteSession(currentSession.id).catch(() => undefined); }); }}>Delete chat</button></ChatActionsMenu> : null}
          <WorkspaceOverview
            key={activeWorkspaceRoot ?? "no-workspace"}
            root={activeWorkspaceRoot}
            name={workspaces.find((workspace) => normalizeRoot(workspace.root) === normalizeRoot(activeWorkspaceRoot))?.name ?? "Workspace"}
            agents={sessions.filter((session) => session.sessionKind === "subagent" && session.parentSessionId === currentSessionId)}
            onFiles={() => workspacePanel.actions.openWorkspaceView("files")}
            onReview={() => workspacePanel.actions.openWorkspaceView("review")}
            onAgent={workspacePanel.actions.openAgentSession}
            sessionId={currentSessionId}
            onTasks={workspacePanel.actions.openTasks}
          />
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
          </div> : null}
        </header>
            {!chatWorkspaceRoot && workspaceBootstrapPending ? <section className="thinGetStarted" role="status">Opening workspace…</section> : !chatWorkspaceRoot ? (
              <section className="thinGetStarted">
                <h1>Get Started</h1>
                <ol>
                  <li>Open a project using File → Open Folder</li>
                  <li>Add models via the <button type="button" onClick={() => setActiveModal("models")}>Models</button> in Settings</li>
                </ol>
              </section>
            ) : hasSelectableModel === false ? (
              <section className="thinGetStarted">
                <h1>Configure a model</h1>
                <p>Use <button type="button" onClick={() => setActiveModal("models")}>Models</button> in Settings to connect a provider.</p>
              </section>
            ) : (
              <ChatArea
                currentSession={currentSession}
                currentSessionId={currentSessionId}
                workspaceName={chatWorkspace?.name ?? "Workspace"}
                workspaceRoot={chatWorkspaceRoot}
                gitStatus={chatUsesActiveWorkspace ? gitStatus : null}
                gitStatusError={chatUsesActiveWorkspace ? gitStatusError : ""}
                githubCliStatus={githubCliStatus}
                runtimeConfigRevision={runtimeConfigRevision}
                onOpenWorkspacePath={workspacePanel.actions.openFilePath}
                onOpenAgentSession={workspacePanel.actions.openAgentSession}
                onNewSession={newChat}
                onOpenResource={setActiveModal}
                onSessionResolved={resolveSession}
                onAgentRunningChange={setRunning}
                onSessionCompleted={completeSession}
              />
            )}
          </main>

          {isFilePaneVisible ? <WorkspaceSplitHandle value={workspacePanelRatio} onChange={setWorkspacePanelRatio} /> : null}
          <aside id="workspace-preview" className={`thinFilePane ${isFilePaneVisible ? "is-open" : ""}`} aria-label="Preview" aria-hidden={!isFilePaneVisible}>
            {workspacePanel.tabs.length > 0 ? (
              <SummaryPanel
                workspaceRoot={activeWorkspaceRoot}
                sessionId={currentSessionId}
                onFiles={() => workspacePanel.actions.openWorkspaceView("files")}
                onReview={() => workspacePanel.actions.openWorkspaceView("review")}
                onTerminal={workspacePanel.actions.openTerminal}
                visible={isFilePaneVisible}
                onRefreshFile={workspacePanel.actions.refreshActiveFile}
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
        {page === "schedules" ? <SchedulesPage onSession={id => void handleSelectSession(id)} /> : null}
        {page === "plugins" ? <section className="managementPage resourcePage"><div className="resourceTabs" hidden={resourceTab === "plugins" && !!pluginId} role="tablist" aria-label="Extensions">{(["plugins", "skills"] as const).map(tab => <button key={tab} role="tab" id={`resource-${tab}`} aria-controls="resource-content" aria-selected={resourceTab === tab} tabIndex={resourceTab === tab ? 0 : -1} onClick={() => setResourceTab(tab)} onKeyDown={event => { if (["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) { event.preventDefault(); const next = event.key === "Home" ? "plugins" : event.key === "End" ? "skills" : resourceTab === "plugins" ? "skills" : "plugins"; setResourceTab(next); document.getElementById(`resource-${next}`)?.focus(); } }}>{tab === "plugins" ? "Plugins" : "Skill"}</button>)}</div><div id="resource-content" role="tabpanel" aria-labelledby={`resource-${resourceTab}`}>{resourceTab === "plugins" ? <PluginsDialog selectedPluginId={pluginId} onSelect={id => { void navigation.move({page: "plugins", resourceTab: "plugins", pluginId: id, sessionId: currentSessionId, root: activeWorkspaceRoot}); }} /> : <SkillsDialog workspaceRoot={activeWorkspaceRoot} confirmAction={confirmAction} />}</div></section> : null}
        {page === "settings" ? <SettingsPage><ModelsDialog onClose={() => navigatePage("chat")} onConfigured={setHasSelectableModel} confirmAction={confirmAction} /></SettingsPage> : null}
        {page === "about" ? <section className="managementPage"><h1>Centaeris</h1><p>Local agent workspace</p><ThemeToggle /></section> : null}
      </div>

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
