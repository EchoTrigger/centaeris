
import { useTranslation } from "../i18n";
import { useEffect, useState } from "react";
import { useLocation, useRouteLoaderData } from "react-router";
import { PanelLeft } from "lucide-react";
import { ShellSidebar } from "./ShellSidebar";
import { apiJson } from "../api";
import { groupSessionNavigation, sortSessionNavigation } from "./sessionNavigation.mjs";
import { SessionNavigationRow } from "./SessionNavigationRow";
import { useSessionNavigationActions } from "./useSessionNavigationActions";
import { ConfirmDialog } from "../components/ConfirmDialog";

/** @param {{ children: import("react").ReactNode, initialTab?: string, activeAgent?: import("../router").AgentSummary | null, mainClassName?: string, showRecentSessions?: boolean, recentRevision?: string }} props */
export function ShellPage({ children, initialTab = "home", activeAgent = null, mainClassName = "", showRecentSessions = false, recentRevision = "" }) {
  const { t } = useTranslation();
  const location = useLocation();
  const { workspace, agents } = useRouteLoaderData("workspace");
  const [sidebarOpen, setSidebarOpen] = useState(() => !showRecentSessions || typeof window === "undefined" || !window.matchMedia("(max-width: 760px)").matches);
  const [recent, setRecent] = useState({ scope: "", sessions: [], projects: [], loading: true, error: false });
  const [retry, setRetry] = useState(0);
  const [actionError, setActionError] = useState("");
  const selectedAgentId = activeAgent?.id || "";
  const scope = JSON.stringify([workspace.id, activeAgent?.id || ""]);
  const refreshKey = JSON.stringify([recentRevision, retry]);
  useEffect(() => {
    if (!showRecentSessions) return undefined;
    const controller = new AbortController();
    // This existing workspace projection excludes coordination Sessions. Keep
    // each record's owner identity; never substitute the selected Agent's id.
    const query = new URLSearchParams({ agentId: selectedAgentId });
    void Promise.all([
      apiJson(`/api/workspaces/${encodeURIComponent(workspace.id)}/sessions?${query}`, { signal: controller.signal }),
      apiJson(`/api/workspaces/${encodeURIComponent(workspace.id)}/session-projects?${query}`, { signal: controller.signal }),
    ]).then(([result, projectData]) => {
        if (controller.signal.aborted) return;
        if (!Array.isArray(projectData.projects) || projectData.projects.some(row => !row || row.workspaceId !== workspace.id || row.agentId !== selectedAgentId || typeof row.id !== "string" || !row.id || typeof row.name !== "string" || !row.name) || !Array.isArray(result.sessions) || result.sessions.some(row => !row || row.workspaceId !== workspace.id || row.status !== "active"
          || typeof row.id !== "string" || !row.id.trim() || row.id.trim() !== row.id
          || typeof row.agentId !== "string" || !row.agentId.trim() || row.agentId.trim() !== row.agentId
          || typeof row.title !== "string" || !row.title.trim() || typeof row.updatedAt !== "string" || !Number.isFinite(Date.parse(row.updatedAt)))) throw new Error("session_navigation_response_invalid");
        if (new Set(result.sessions.map(row => row.id)).size !== result.sessions.length) throw new Error("session_navigation_response_invalid");
        setRecent({ scope, refreshKey, projects: projectData.projects, sessions: sortSessionNavigation(result.sessions), loading: false, error: false });
      })
      .catch(() => { if (!controller.signal.aborted) setRecent({ scope, refreshKey, sessions: [], projects: [], loading: false, error: true }); });
    return () => controller.abort();
  }, [showRecentSessions, workspace.id, selectedAgentId, scope, refreshKey]);
  const recentNavigation = showRecentSessions ? {
    ...(recent.scope === scope ? recent : { sessions: [], projects: [], loading: true, error: false }), onRetry: () => setRetry(value => value + 1),
  } : undefined;
  const sessionActions = useSessionNavigationActions({ scopeKey: scope, sessions: recentNavigation?.sessions || [], onError: setActionError,
    onChangeSessions: update => setRecent(previous => previous.scope === scope ? { ...previous, sessions: update(previous.sessions) } : previous),
  });
  const sessionNavigation = recentNavigation ? {
    agentId: selectedAgentId, sessions: recentNavigation.sessions, projects: recentNavigation.projects,
    groupedSessions: groupSessionNavigation(recentNavigation.sessions, recentNavigation.projects),
    renderSessionRow: (session, options = {}) => <SessionNavigationRow key={session.id} workspaceId={workspace.id} session={session} actions={sessionActions} {...options} />,
    onCreateProject: async name => {
      const data = await apiJson(`/api/workspaces/${encodeURIComponent(workspace.id)}/session-projects`, { method: "POST", body: JSON.stringify({ agentId: selectedAgentId, name }) });
      if (!data?.project || data.project.workspaceId !== workspace.id || data.project.agentId !== selectedAgentId) throw new Error("session_project_response_invalid");
      setRetry(value => value + 1);
      return data.project;
    },
  } : undefined;
  return (
    <div className={`shShellPage ${showRecentSessions ? "agentChatShell" : ""} ${sidebarOpen ? "isSidebarOpen" : "isSidebarClosed"}`}>
      <a className="shSkipLink" href="#workspace-main">{t("shellPage.skipToMainContent")}</a>
      <ShellSidebar workspace={workspace} agents={agents} activeAgent={activeAgent} recentNavigation={recentNavigation} sessionProps={sessionNavigation} initialTab={location.state?.sidebarTab || initialTab} onCollapse={() => setSidebarOpen(false)} />
      <main className={`shMain ${mainClassName}`} id="workspace-main" tabIndex="-1">
        {!sidebarOpen ? <button className="shSidebarOpen" type="button" aria-label={t("appRoute.showSidebar")} title={t("appRoute.showSidebar")} onClick={() => setSidebarOpen(true)}><PanelLeft aria-hidden="true" /></button> : null}
        {actionError ? <p className="shEmptyHint" role="alert">{actionError}</p> : null}
        {children}
      </main>
      <ConfirmDialog open={Boolean(sessionActions.deleteConfirmationSessionId)} title={t("appRoute.deleteThisConversation")} busy={Boolean(sessionActions.deletingSessionId)}
        onCancel={() => sessionActions.setDeleteConfirmationSessionId("")} onConfirm={() => sessionActions.deleteSession(sessionActions.deleteConfirmationSessionId)} />
    </div>
  );
}
