import { Menu } from "@centaeris/frontend-foundations/primitives/menu";
import { useTranslation } from "../i18n";
import { useState } from "react";
import { createPortal } from "react-dom";
import { Link, useLocation, useNavigate, useRouteLoaderData } from "react-router";
import {
  ChevronDown,
  ChevronRight,
  Blocks,
  Folder,
  Library,
  PanelLeft,
  Plus,
  Search,
  SquarePen,
} from "lucide-react";
import { apiResponse, clearCsrfToken } from "../api";
import { useModalDialog } from "../components/useModalDialog";
import { WorkspaceAvatar } from "../components/WorkspaceAvatar";
import { clearWorkspaceIdentityCache } from "./workspaceIdentityCache";

function WorkspaceHeader({ workspace, workspaces, logoutBusy, logoutError, onLogout, onCollapse, search, returnTo, menuContainer }) {
  const { t } = useTranslation();
  const base = `/w/${encodeURIComponent(workspace.id)}`;
  const otherWorkspaces = workspaces.filter((item) => item.id !== workspace.id);

  return (
    <div className="shWs">
      <Menu.Root modal={false}>
        <Menu.Trigger className="shWsButton" aria-label={t("shellSidebar.valueWorkspaceMenu", { value1: workspace.name })}>
          <WorkspaceAvatar workspace={workspace} />
          <span className="shWsName">{workspace.name}</span>
          <ChevronDown aria-hidden="true" />
        </Menu.Trigger>
        <Menu.Portal container={menuContainer}>
          <Menu.Positioner align="start" sideOffset={3} className="shWsMenuPositioner">
            <Menu.Popup className="shWsMenuPopover" aria-label={t("shellSidebar.valueWorkspaceActions", { value1: workspace.name })}>
              <Menu.LinkItem className="shWsMenuItem" closeOnClick render={<Link to={`${base}/settings/preferences`} state={{ returnTo }} />}>
                {t("shellSidebar.settings")}
              </Menu.LinkItem>
              {otherWorkspaces.length ? <Menu.SubmenuRoot>
                <Menu.SubmenuTrigger className="shWsMenuItem shWsMenuSwitch" openOnHover={!menuContainer}>
                  {t("shellSidebar.switchWorkspace")}<ChevronRight aria-hidden="true" />
                </Menu.SubmenuTrigger>
                <Menu.Portal>
                  <Menu.Positioner side="right" align="start" sideOffset={7} alignOffset={-5} className="shWsMenuPositioner">
                    <Menu.Popup className="shWsMenuPopover shWsMenuWorkspaces" aria-label={t("shellSidebar.switchWorkspace")}>
                      {otherWorkspaces.map(item => <Menu.LinkItem key={item.id} className="shWsMenuItem shWsMenuWorkspace" closeOnClick
                        render={<Link to={`/w/${encodeURIComponent(item.id)}/app`} />}>
                        <WorkspaceAvatar workspace={item} />
                        <span>{item.name}</span>
                      </Menu.LinkItem>)}
                    </Menu.Popup>
                  </Menu.Positioner>
                </Menu.Portal>
              </Menu.SubmenuRoot> : null}
              <Menu.Separator className="shWsMenuDivider" />
              <Menu.Item className="shWsMenuItem" disabled={logoutBusy} closeOnClick={false} onClick={onLogout}>
                {logoutBusy ? t("shellSidebar.signingOut") : t("shellSidebar.signOut")}
              </Menu.Item>
              {logoutError ? <p className="shWsMenuError" role="alert">{logoutError}</p> : null}
            </Menu.Popup>
          </Menu.Positioner>
        </Menu.Portal>
      </Menu.Root>
      {search ? <button className="shSearchButton" type="button" ref={search.triggerRef} aria-pressed={search.open}
        aria-label={t("searchOverlay.searchConversationsAndNotes")} title={t("shellSidebar.searchCtrlK")} onClick={search.toggle}><Search aria-hidden="true" /></button> : null}
      {onCollapse ? <button className="shSidebarCollapse shAvatarToggle" type="button" aria-label={t("shellSidebar.hideSidebar")} title={t("shellSidebar.hideSidebar")} onClick={onCollapse}><PanelLeft aria-hidden="true" /></button> : null}
    </div>
  );
}

function ProjectCreateDialog({ onClose, onCreate }) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const dialogRef = useModalDialog({ onClose });

  async function submit(event) {
    event.preventDefault();
    const normalizedName = name.trim();
    if (!normalizedName || busy) return;
    setBusy(true);
    setError("");
    try {
      await onCreate(normalizedName);
      onClose();
    } catch {
      setError(t("shellSidebar.unableToCreateProjectPleaseTryAgain"));
      setBusy(false);
    }
  }

  return createPortal(<div className="shPrivateCreateBackdrop" role="presentation" onMouseDown={onClose}>
    <form className="shProjectCreateDialog" ref={dialogRef} role="dialog" aria-modal="true" aria-labelledby="shProjectCreateTitle" tabIndex={-1} onSubmit={submit} onMouseDown={(event) => event.stopPropagation()}>
      <header><button className="quietCloseButton" type="button" aria-label={t("shellSidebar.closeProjectCreation")} onClick={onClose}><X aria-hidden="true" /></button><h2 id="shProjectCreateTitle">{t("shellSidebar.createProject")}</h2></header>
      <label>
        <span>{t("shellSidebar.projectName")}</span>
        <input className="shProjectNameInput" autoFocus maxLength={100} value={name} onChange={(event) => setName(event.target.value)} />
      </label>
      {error ? <p role="alert">{error}</p> : null}
      <footer><button type="button" onClick={onClose}>{t("agentRunRow.cancel")}</button><button className="isPrimary" type="submit" disabled={!name.trim() || busy}>{busy ? t("shellSidebar.creatingProject") : t("shellSidebar.createProject")}</button></footer>
    </form>
  </div>, document.body);
}

function ConversationSections({ base, sessionProps, recentNavigation, onStartNewChat }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const startNewChat = (projectId = "") => {
    if (onStartNewChat) onStartNewChat(projectId);
    else {
      const query = new URLSearchParams({ new: "1" });
      if (projectId) query.set("projectId", projectId);
      navigate(`${base}/app?${query}`);
    }
  };
  return (
    <div className="shScroll">
      {sessionProps ? <>
        <section className="shSection shCollapsibleSection shSessionSection">
          <details className="shDisclosure" open>
            <summary className="shSectionHeader shDisclosureSummary"><span>{t("appRoute.pin")}</span><ChevronDown aria-hidden="true" /></summary>
            <nav className="workspaceSessionList" aria-label={t("shellSidebar.pinnedConversations")}>
              {sessionProps.groupedSessions.pinned.map((session) => sessionProps.renderSessionRow(session, { icon: true }))}
            </nav>
          </details>
        </section>

        <section className="shSection shCollapsibleSection shSessionSection">
          <details className="shDisclosure" open>
            <summary className="shSectionHeader shDisclosureSummary"><span>{t("shellSidebar.projects")}</span><ChevronDown aria-hidden="true" /></summary>
            <div className="shProjectTree">
              {sessionProps.projects.map((project) => <section className="shProject" key={project.id}>
                <details className="shProjectDisclosure" open>
                  <summary className="shProjectSummary"><ChevronDown aria-hidden="true" /><Folder aria-hidden="true" /><span>{project.name}</span></summary>
                  <nav className="workspaceSessionList isProject" aria-label={t("shellSidebar.valueConversations", { value1: project.name })}>
                    {sessionProps.groupedSessions.projectSessions[project.id].map((session) => sessionProps.renderSessionRow(session, { nested: true }))}
                  </nav>
                </details>
                <button className="shProjectAction" type="button" aria-label={t("shellSidebar.newConversationInValue", { value1: project.name })} onClick={() => startNewChat(project.id)}><Plus aria-hidden="true" /></button>
              </section>)}
            </div>
          </details>
          <button className="shSectionAction" type="button" aria-label={t("shellSidebar.createProject")} onClick={sessionProps.onOpenProjectCreate}><Plus aria-hidden="true" /></button>
        </section>

        <section className="shSection shCollapsibleSection shSessionSection">
          <details className="shDisclosure" open>
            <summary className="shSectionHeader shDisclosureSummary"><span>{t("shellSidebar.recent")}</span><ChevronDown aria-hidden="true" /></summary>
            <nav className="workspaceSessionList" aria-label={t("shellSidebar.recentConversations")}>
              {sessionProps.groupedSessions.recent.map((session) => sessionProps.renderSessionRow(session))}
            </nav>
          </details>
        </section>
      </> : null}
      {recentNavigation?.loading ? <p className="shEmptyHint" role="status">{t("agentChat.loading")}</p> : null}
      {recentNavigation?.error ? <p className="shEmptyHint" role="alert">
        {t(recentNavigation.onRetry ? "agentChat.recentError" : "appRoute.unableToLoadConversationsRefreshThePageAndTry")}
        {recentNavigation.onRetry ? <button type="button" onClick={recentNavigation.onRetry}>{t("agentChat.retry")}</button> : null}
      </p> : null}
    </div>
  );
}

export function ShellSidebar({ workspace, sessionProps, recentNavigation, onStartNewChat, onCollapse, search, menuContainer }) {
  const { t } = useTranslation();
  const [logoutBusy, setLogoutBusy] = useState(false);
  const [logoutError, setLogoutError] = useState("");
  const [projectCreateOpen, setProjectCreateOpen] = useState(false);
  const { workspaces = [workspace] } = useRouteLoaderData("workspace");
  const location = useLocation();
  const navigate = useNavigate();
  const base = `/w/${encodeURIComponent(workspace.id)}`;
  const inSettings = location.pathname.startsWith(`${base}/settings/`);
  const isHome = location.pathname === `${base}/app`;
  const inLibrary = location.pathname === `${base}/library` || location.pathname.startsWith(`${base}/library/`);

  const inExtensions = location.pathname === base + "/extensions";

  function newConversation() {
    if (isHome) {
      if (search?.open || onCollapse) {
        if (search?.open) search.close();
        else onCollapse();
        window.requestAnimationFrame(() => document.getElementById("messageDraft")?.focus());
      } else {
        document.getElementById("messageDraft")?.focus();
      }
      return;
    }
    if (onStartNewChat) onStartNewChat();
    else navigate(base + "/app?new=1");
  }

  async function logout() {
    if (logoutBusy) return;
    setLogoutBusy(true);
    setLogoutError("");
    try {
      await apiResponse("/api/logout", { method: "POST" });
      clearWorkspaceIdentityCache();
      clearCsrfToken();
      navigate("/login", { replace: true });
    } catch {
      setLogoutError(t("shellSidebar.unableToSignOutPleaseTryAgain"));
      setLogoutBusy(false);
    }
  }

  if (inSettings) return null;

  return (
    <aside className="workspaceSidebar shSidebar" aria-label={t("shellSidebar.conversationNavigation")}>
      <WorkspaceHeader workspace={workspace} workspaces={workspaces} logoutBusy={logoutBusy} logoutError={logoutError} onLogout={logout} menuContainer={menuContainer}
        search={search} onCollapse={onCollapse} returnTo={location.pathname + location.search + location.hash} />
      <nav className="shPrimaryNav" aria-label={t("shellSidebar.mainView")}>
        <button className="shRow" type="button" onClick={newConversation}>
          <SquarePen aria-hidden="true" /><span>{t("shellSidebar.newConversation")}</span>
        </button>
        <Link className="shRow" to={base + "/library"} aria-current={inLibrary ? "page" : undefined}>
          <Library aria-hidden="true" /><span>{t("libraryObjectRoute.library")}</span>
        </Link>
        <Link className="shRow" to={base + "/extensions"} aria-current={inExtensions ? "page" : undefined}>
          <Blocks aria-hidden="true" /><span>{t("extensions.title")}</span>
        </Link>
      </nav>

      <ConversationSections base={base} sessionProps={sessionProps ? { ...sessionProps, onOpenProjectCreate: () => setProjectCreateOpen(true) } : null} recentNavigation={recentNavigation} onStartNewChat={onStartNewChat} />

      {projectCreateOpen ? <ProjectCreateDialog onClose={() => setProjectCreateOpen(false)} onCreate={sessionProps.onCreateProject} /> : null}
    </aside>
  );
}
