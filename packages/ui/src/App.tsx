import { DesktopShell } from "./components/DesktopShell";
import { ChatActionsMenu } from "./components/ChatActionsMenu";
import { DropdownMenuItem } from "./components/ui/dropdown-menu";
import { SettingsNavigation, SettingsPage, type SettingsSection } from "./components/SettingsPage";
import { ApplicationBar } from "./components/ApplicationBar";
import { SchedulesPage } from "./components/SchedulesPage";
import { useAppNavigation, type AppPage } from "./components/app/useAppNavigation";
import { listenHost } from "./host/hostBridge";
import { t } from "./i18n";
import { useCallback, useEffect, useRef, useState } from "react";
import { PanelRight, X, Settings2, LoaderCircle, Folder } from "lucide-react";
import { Button } from "./components/ui/button";
import { Dialog, DialogContent, DialogTitle } from "./components/ui/dialog";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "./components/ui/tabs";
import "./styles/desktop-shell.css";
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
import { WorkspaceLayout } from "./components/WorkspaceLayout";
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
  const [newChatRevision, setNewChatRevision] = useState(0);
  const [page, setPage] = useState<AppPage>("chat");
  const [composerResource, setComposerResource] = useState<ResourceModalKind | null>(null);
  const [settingsSection, setSettingsSection] = useState<SettingsSection>("appearance");
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
    isChatVisible: page === "chat",
    reportError: setHostError,
  });
  const {
    sessions,
    currentSessionId,
    currentSession,
    mainSessions,
    runningSessionIds,
    completedSessionIds,
    waitingSessionIds,
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
    setWaiting,
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
    ]).then(([, sessionResult]) => {
      if (cancelled) return;
      if (sessionResult.status === "fulfilled") {
        sessionInitialization.applySessions(sessionResult.value);
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
    setComposerResource(null);
    setPage(location.page);
    return true;
  });
  const navigatePage = (next: AppPage) => { if (next === "settings") setSettingsSection("appearance"); void navigation.move({ page: next, sessionId: currentSessionId, root: activeWorkspaceRoot }); };
  const setActiveModal = (kind: AppModal) => {
    if (!kind) { navigatePage("chat"); return; }
    if (kind === "models") { navigatePage("settings"); setSettingsSection("models"); }
    else { setResourceTab(kind); navigatePage("plugins"); }
  };
  const handleOpenWorkspace = async (mode: Parameters<typeof openWorkspace>[0]) => {
    const origin = { page, pluginId, resourceTab, sessionId: currentSessionId, root: activeWorkspaceRoot };
    const snapshot = await openWorkspace(mode);
    if (!snapshot) return;
    navigation.accept({ page: "chat", sessionId: null, root: snapshot.activeWorkspaceRoot ?? null }, origin);
    setComposerResource(null); clearSelection(); clearWorkspacePanel(); setPage("chat");
  };
  const handleSelectWorkspace = async (root: string) => navigation.move({ page: "chat", sessionId: null, root });
  const handleSelectSession = async (sessionId: string) => { await navigation.move({ page: "chat", sessionId, root: sessions.find(s => s.id === sessionId)?.cwd ?? activeWorkspaceRoot }); };
  const newChat = () => { setNewChatRevision(revision => revision + 1); void navigation.move({ page: "chat", sessionId: null, root: activeWorkspaceRoot }); };
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
    <DesktopShell expanded={isSidebarOpen} className={`thinAppShell ${isSidebarOpen ? "is-sidebar-open" : "is-sidebar-collapsed"}`}>
      <ApplicationBar expanded={isSidebarOpen} onToggle={() => setIsSidebarOpen(open => !open)} canBack={navigation.canBack} canForward={navigation.canForward} onBack={() => void navigation.move(-1)} onForward={() => void navigation.move(1)} onError={setHostError} />

      <div className="thinSidebarSlot" inert={!isSidebarOpen}>
        <Sidebar
          onExpandWorkspace={sessionController.actions.expandWorkspace}
          onLoadMore={sessionController.actions.loadMore}
          hasMore={sessionController.hasMore}
          activePage={page}
          collapsed={!isSidebarOpen || page === "settings"}
          onNavigate={navigatePage}
          onPinSession={pinSession}
          onReorderPinnedSessions={reorderPinnedSessions}
          sessions={mainSessions}
          currentSessionId={currentSessionId}
          workspaces={workspaces}
          activeWorkspaceRoot={activeWorkspaceRoot}
          runningSessionIds={runningSessionIds}
          completedSessionIds={completedSessionIds}
          waitingSessionIds={waitingSessionIds}
          workspaceCatalogError={workspaceCatalogError}
          onNewChat={newChat}
          onOpenWorkspace={handleOpenWorkspace}
          onRenameWorkspace={renameWorkspace}
          onRemoveWorkspace={removeWorkspace}
          onSelectWorkspace={handleSelectWorkspace}
          onRetryWorkspaceCatalog={retryCatalog}
          onResetWorkspaceCatalog={handleResetWorkspaceCatalog}
          onSelectSession={(sessionId) => void handleSelectSession(sessionId)}
          onRenameSession={renameSession}
          onDeleteSession={handleDeleteSession}
          onOpenResource={setActiveModal}
          onOpenFile={workspacePanel.actions.openFile}
        />
        {page === "settings" ? <SettingsNavigation section={settingsSection} onSelect={setSettingsSection} collapsed={!isSidebarOpen} /> : null}
      </div>
      <div className={`thinWorkspaceShell ${page === "chat" ? "is-chat-page" : ""}`}>

        <WorkspaceLayout hidden={page !== "chat"} previewVisible={isFilePaneVisible} preview={
          <aside id="workspace-preview" className={`thinFilePane ${isFilePaneVisible ? "is-open" : ""}`} aria-label="Preview" aria-hidden={!isFilePaneVisible}>
            {workspacePanel.tabs.length > 0 ? (
              <SummaryPanel
                workspaceRoot={activeWorkspaceRoot}
                sessionId={currentSessionId}
                onFiles={(id) => workspacePanel.actions.openWorkspaceView("files", id)}
                onReview={(id) => workspacePanel.actions.openWorkspaceView("review", id)}
                onTerminal={workspacePanel.actions.openTerminal}
                onAddTab={workspacePanel.actions.openNewTab}
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
        }>
          <main className="thinChatColumn">
        <header className={`workspacePageHeader desktopConversationHeader ${currentSessionId ? "" : "is-welcome"}`} aria-label={currentSessionId ? "Conversation" : "Workspace actions"}>
          {currentSessionId ? <div className="desktopConversationIdentity"><strong role="heading" aria-level={1} title={currentSession?.title}>{currentSession?.title ?? "Conversation"}</strong>
            <span aria-label="Conversation workspace" title={chatWorkspaceRoot ?? undefined}><Folder size={12} aria-hidden="true" />{chatWorkspace?.name ?? chatWorkspaceRoot?.split(/[\\/]/).filter(Boolean).at(-1) ?? "Workspace"}</span>
          </div> : null}
          {page === "chat" ? <div className="workspacePageActions">
            {currentSession ? <ChatActionsMenu><DropdownMenuItem onClick={() => void pinSession(currentSession.id, !currentSession.isPinned).catch(() => undefined)}>{currentSession.isPinned ? "Unpin chat" : "Pin chat"}</DropdownMenuItem><DropdownMenuItem onClick={() => { void confirmAction({ title: "Delete chat?", message: currentSession.title }).then(confirmed => { if (confirmed) void handleDeleteSession(currentSession.id).catch(() => undefined); }); }}>Delete chat</DropdownMenuItem></ChatActionsMenu> : null}
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
          {!isFilePaneVisible && (workspacePanel.tabs.length > 0 || activeWorkspaceRoot) ? (
            <button
              type="button"
              className="nativePanelToggle is-right"
              onClick={() => workspacePanel.tabs.length ? workspacePanel.actions.show() : workspacePanel.actions.openNewTab()}
              aria-label="Show right sidebar"
              aria-expanded={false}
            >
              <PanelRight aria-hidden="true" />
            </button>
          ) : null}
          </div> : null}
        </header>
            {!chatWorkspaceRoot && workspaceBootstrapPending ? <section className="desktopSetupState" role="status"><LoaderCircle className="desktopLoadingIcon" aria-hidden="true" /><p>Opening your workspace…</p></section> : chatWorkspaceRoot && hasSelectableModel === false ? (
              <section className="desktopSetupState">
                <Settings2 size={32} strokeWidth={1.4} aria-hidden="true" />
                <h1>Connect a model to get started</h1>
                                <Button onClick={() => setActiveModal("models")}><Settings2 aria-hidden="true" /> Configure a model</Button>
              </section>
            ) : null}
              <ChatArea
                hidden={page !== "chat" || (!chatWorkspaceRoot && workspaceBootstrapPending) || Boolean(chatWorkspaceRoot && hasSelectableModel === false)}
                workspaces={workspaces}
                onOpenWorkspace={() => handleOpenWorkspace("customPath")}
                onSelectWorkspace={handleSelectWorkspace}
                newChatRevision={newChatRevision}
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
                onOpenResource={setComposerResource}
                onSessionResolved={resolveSession}
                onAgentRunningChange={setRunning}
                onSessionWaitingChange={setWaiting}
                onSessionCompleted={completeSession}
              />
          </main>

        </WorkspaceLayout>
        {page === "schedules" ? <SchedulesPage onSession={id => void handleSelectSession(id)} onCreate={newChat} /> : null}
        {page === "plugins" ? <Tabs value={resourceTab} onValueChange={value => { if (value === "plugins" || value === "skills") setResourceTab(value); }} className="desktopResourcesPage managementPage resourcePage">
          <TabsList className="desktopResourceTabs" variant="line" aria-label="Extensions"><TabsTrigger value="plugins">Plugins</TabsTrigger><TabsTrigger value="skills">Skills</TabsTrigger></TabsList>
          <TabsContent value="plugins"><PluginsDialog selectedPluginId={pluginId} onSelect={id => { void navigation.move({page: "plugins", resourceTab: "plugins", pluginId: id, sessionId: currentSessionId, root: activeWorkspaceRoot}); }} /></TabsContent>
          <TabsContent value="skills"><SkillsDialog workspaceRoot={activeWorkspaceRoot} confirmAction={confirmAction} /></TabsContent>
        </Tabs> : null}
        {page === "settings" ? <SettingsPage section={settingsSection}><ModelsDialog onClose={() => navigatePage("chat")} onConfigured={setHasSelectableModel} confirmAction={confirmAction} /></SettingsPage> : null}
        {page === "about" ? <section className="managementPage"><h1>Centaeris</h1><p>Local agent workspace</p></section> : null}
      </div>

      <ConfirmDialog
        open={Boolean(confirmation)}
        title={confirmation?.title ?? ""}
        message={confirmation?.message}
        onCancel={() => answerConfirmation(false)}
        onConfirm={() => answerConfirmation(true)}
      />

      <Dialog open={composerResource !== null} onOpenChange={open => { if (!open) setComposerResource(null); }}>
        <DialogContent className="desktopResourceDialog" finalFocus={() => document.getElementById("message-input")}>
          <DialogTitle className="sr-only">{composerResource === "models" ? "Model services" : composerResource === "skills" ? "Skills" : "Plugins"}</DialogTitle>
          <div className="desktopResourceDialogBody">
            {composerResource === "models" ? <ModelsDialog onClose={() => setComposerResource(null)} onConfigured={setHasSelectableModel} confirmAction={confirmAction} /> : composerResource === "skills" ? <SkillsDialog workspaceRoot={chatWorkspaceRoot} confirmAction={confirmAction} /> : composerResource === "plugins" ? <PluginsDialog /> : null}
          </div>
        </DialogContent>
      </Dialog>

      {hostError ? (
        <div className="thinHostError desktopHostNotice" role="alert"><span>{hostError}</span><Button variant="ghost" size="icon-sm" aria-label="Dismiss notification" onClick={() => setHostError("")}><X aria-hidden="true" /></Button></div>
      ) : null}
    </DesktopShell>
  );
}

export default App;
