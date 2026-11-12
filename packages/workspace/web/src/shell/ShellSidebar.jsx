import { Menu } from "@centaeris/ui/primitives/menu";
import { useTranslation } from "../i18n";
import { memo, useState } from "react";
import { Link, useLocation, useNavigate, useRouteLoaderData } from "react-router";
import {
  ChevronDown,
  ChevronRight,
  Blocks,
  Folder,
  Library,
  PanelLeft,
  Search,
  SquarePen,
  ListTodo,
  House,
  MessageSquare,
} from "lucide-react";
import { displayWorkText, exampleProjects, useWorkPrototype } from "../work/workPrototype";
import { apiResponse, clearCsrfToken } from "../api";
import { WorkspaceAvatar } from "../components/WorkspaceAvatar";
import { sessionChatPath } from "./sessionNavigation.mjs";
import { clearWorkspaceIdentityCache } from "./workspaceIdentityCache";

function WorkspaceHeader({ onNewConversation, workspace, workspaces, logoutBusy, logoutError, onLogout, onCollapse, search, returnTo, menuContainer }) {
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
                        render={<Link to={`/w/${encodeURIComponent(item.id)}/work`} />}>
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
      <button className="shSearchButton" type="button" aria-label={t("shellSidebar.newConversation")} title={t("shellSidebar.newConversation")} onClick={onNewConversation}><SquarePen aria-hidden="true" /></button>
      {onCollapse ? <button className="shSidebarCollapse shAvatarToggle" type="button" aria-label={t("shellSidebar.hideSidebar")} title={t("shellSidebar.hideSidebar")} onClick={onCollapse}><PanelLeft aria-hidden="true" /></button> : null}
    </div>
  );
}

export const ShellSidebar = memo(function ShellSidebar({ workspace, onStartNewChat, onCollapse, search, menuContainer, sessionProps }) {
  const work = useWorkPrototype();
  const { t } = useTranslation();
  const [logoutBusy, setLogoutBusy] = useState(false);
  const [logoutError, setLogoutError] = useState("");
  const { workspaces = [workspace] } = useRouteLoaderData("workspace");
  const location = useLocation();
  const navigate = useNavigate();
  const base = `/w/${encodeURIComponent(workspace.id)}`;
  const inSettings = location.pathname.startsWith(`${base}/settings/`);
  const isHome = location.pathname === `${base}/app`;
  const inLibrary = location.pathname === `${base}/library` || location.pathname.startsWith(`${base}/library/`);

  const favoriteSessions = (sessionProps?.sessions || []).filter(session => session.isPinned);
  const favoriteProjects = exampleProjects.filter(project => work?.favorites.includes(project.id));

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
      <WorkspaceHeader onNewConversation={newConversation} workspace={workspace} workspaces={workspaces} logoutBusy={logoutBusy} logoutError={logoutError} onLogout={logout} menuContainer={menuContainer}
        search={search} onCollapse={onCollapse} returnTo={location.pathname + location.search + location.hash} />
      <nav className="shPrimaryNav workPrimaryNav" aria-label={t("shellSidebar.mainView")}>
        <Link className="shRow" to={`${base}/work`} aria-current={location.pathname === `${base}/work` && !location.search ? "page" : undefined}><House aria-hidden="true" /><span>{t("work.myWork")}</span></Link>
        <Link className="shRow" to={`${base}/work/projects`} aria-current={location.pathname.startsWith(`${base}/work/projects`) ? "page" : undefined}><Folder aria-hidden="true" /><span>{t("work.projects")}</span></Link>
        <Link className="shRow" to={`${base}/work/tasks`} aria-current={location.pathname.startsWith(`${base}/work/tasks`) ? "page" : undefined}><ListTodo aria-hidden="true" /><span>{t("work.tasks")}</span></Link>
        <Link className="shRow" to={base + "/extensions"} aria-current={inExtensions ? "page" : undefined}><Blocks aria-hidden="true" /><span>{t("extensions.title")}</span></Link>
        <Link className="shRow" to={base + "/library"} aria-current={inLibrary ? "page" : undefined}><Library aria-hidden="true" /><span>{t("libraryObjectRoute.library")}</span></Link>
        <Link className="shRow" to={`${base}/app`} aria-current={isHome || location.pathname.startsWith(`${base}/sessions/`) ? "page" : undefined}><MessageSquare aria-hidden="true" /><span>Agent</span></Link>
      </nav>
      <nav className="workRailViews" aria-label={t("work.favoritesAndViews")}>
        <p className="workRailHeading">{t("work.favorites")}</p>
        {favoriteProjects.map(project => <Link className="shRow" key={project.id} to={`${base}/work/projects/${project.id}`}><span className="workRailProjectIcon">{project.icon}</span><span>{displayWorkText(project.name, t)}</span></Link>)}
        {favoriteSessions.map(session => <Link className="shRow" key={session.id} to={sessionChatPath(workspace.id, session.id)} aria-current={location.pathname === sessionChatPath(workspace.id, session.id) ? "page" : undefined} title={session.title}><span>{session.title || t("work.conversation.untitled")}</span></Link>)}
        {!favoriteProjects.length && !favoriteSessions.length ? <p className="workRailEmpty">{t("work.favoriteHint")}</p> : null}
        <p className="workRailHeading">{t("work.views")}</p>
        <Link className="shRow" to={`${base}/work?tab=attention`} aria-current={location.pathname === `${base}/work` && location.search.includes("tab=attention") ? "page" : undefined}><ListTodo aria-hidden="true" /><span>{t("work.needsAttention")}</span><span className="workRailCount">{work?.tasks.filter(task => task.owner === "me" && task.attention && !["done", "cancelled"].includes(task.status)).length || 0}</span></Link>
      </nav>

    </aside>
  );
});
