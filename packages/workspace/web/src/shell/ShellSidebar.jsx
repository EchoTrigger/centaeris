import { Menu } from "@centaeris/ui/primitives/menu";
import { useTranslation } from "../i18n";
import { memo, useId, useState } from "react";
import { AnimatePresence, motion, useReducedMotion } from "@centaeris/ui/motion-react";
import { Link, useLocation, useNavigate, useRouteLoaderData } from "react-router";
import {
  ChevronDown,
  ChevronRight,
  Blocks,
  Box,
  LibraryBig,
  PanelLeft,
  Search,
  SquarePen,
  Inbox,
  Focus,
  GitPullRequest,
  House,
  Copy,
  Layers2,
  Infinity,
  MousePointer2,
} from "lucide-react";
import { apiResponse, clearCsrfToken } from "../api";
import { WorkspaceAvatar } from "../components/WorkspaceAvatar";
import { useCollapsedNavigation, useWorkspaceNavigation } from "./workspaceNavigationState";
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

export const ShellSidebar = memo(function ShellSidebar({ workspace, agentPath, onStartNewChat, onCollapse, search, menuContainer, sessionProps }) {
  const { user } = useRouteLoaderData("authenticated");
  const navigation = useWorkspaceNavigation(user.id, workspace.id);
  const collapsed = useCollapsedNavigation(navigation);
  const favoritesOpen = !collapsed.includes("favorites");
  const teamsOpen = !collapsed.includes("teams");
  const reducedMotion = useReducedMotion();
  const favoritesTransition = { duration: reducedMotion ? 0 : 0.2, ease: [0.16, 1, 0.3, 1] };
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
        <div className="shNavGroup">
          <Link className="shRow" to={`${base}/work/inbox`} aria-current={location.pathname === `${base}/work/inbox` ? "page" : undefined}><Inbox aria-hidden="true" /><span>{t("work.inbox")}</span></Link>
          <Link className="shRow" to={`${base}/work`} aria-current={location.pathname === `${base}/work` ? "page" : undefined}><Focus aria-hidden="true" /><span>{t("work.myIssues")}</span></Link>
          <Link className="shRow" to={`${base}/work/reviews`} aria-current={location.pathname === `${base}/work/reviews` ? "page" : undefined}><GitPullRequest aria-hidden="true" /><span>{t("work.reviews")}</span></Link>
          <Link className="shRow" to={base + "/library"} aria-current={inLibrary ? "page" : undefined}><LibraryBig aria-hidden="true" /><span>{t("libraryObjectRoute.library")}</span></Link>
          <Link className="shRow" to={base + "/extensions"} aria-current={inExtensions ? "page" : undefined}><Blocks aria-hidden="true" /><span>{t("extensions.title")}</span></Link>
          <Link className="shRow" to={agentPath || `${base}/app`} aria-current={isHome || location.pathname.startsWith(`${base}/sessions/`) ? "page" : undefined}><MousePointer2 aria-hidden="true" /><span>Agent</span></Link>
        </div>
        <AnimatePresence initial={false}>
        {favoriteSessions.length ? <motion.section key="favorites" className="workRailViews shFavorites"
          initial={{ height: 0, opacity: 0, marginTop: 0 }} animate={{ height: "auto", opacity: 1, marginTop: 18 }} exit={{ height: 0, opacity: 0, marginTop: 0 }} transition={favoritesTransition}>
          <SidebarGroup label={t("work.favorites")} open={favoritesOpen} onToggle={() => navigation.setCollapsed("favorites", favoritesOpen)} transition={favoritesTransition}>
            {favoriteSessions.map(session => <Link className="shRow" key={session.id} to={sessionChatPath(workspace.id, session.id)} aria-current={location.pathname === sessionChatPath(workspace.id, session.id) ? "page" : undefined} title={session.title}
              onClick={event => {
                if (sessionProps?.onOpenSession && event.button === 0 && !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey) {
                  event.preventDefault();
                  sessionProps.onOpenSession(session);
                }
              }}><span>{session.title || t("work.conversation.untitled")}</span></Link>)}
          </SidebarGroup>
        </motion.section> : null}
        </AnimatePresence>
        <section className="shTeams">
          <SidebarGroup label={t("work.yourTeams")} open={teamsOpen} onToggle={() => navigation.setCollapsed("teams", teamsOpen)} transition={favoritesTransition}>
          <Link className="shRow" to={`${base}/work/home`} aria-current={location.pathname === `${base}/work/home` ? "page" : undefined}><House aria-hidden="true" /><span>{t("work.home")}</span></Link>
          <Link className="shRow" to={`${base}/work/tasks`} aria-current={location.pathname.startsWith(`${base}/work/tasks`) ? "page" : undefined}><Copy aria-hidden="true" /><span>{t("work.issues")}</span></Link>
          <Link className="shRow" to={`${base}/work/projects`} aria-current={location.pathname.startsWith(`${base}/work/projects`) ? "page" : undefined}><Box aria-hidden="true" /><span>{t("work.projects")}</span></Link>
          <Link className="shRow" to={`${base}/work/views`} aria-current={location.pathname === `${base}/work/views` ? "page" : undefined}><Layers2 aria-hidden="true" /><span>{t("work.views")}</span></Link>
          <Link className="shRow" to={`${base}/work/loops`} aria-current={location.pathname === `${base}/work/loops` ? "page" : undefined}><Infinity aria-hidden="true" /><span>{t("work.loops")}</span></Link>
          </SidebarGroup>
        </section>
      </nav>

    </aside>
  );
});


function SidebarGroup({ label, open, onToggle, transition, children }) {
  const id = useId();
  return <>
    <button className="shNavGroupToggle" type="button" aria-expanded={open} aria-controls={id} onClick={onToggle}>
      <span>{label}</span><motion.span aria-hidden="true" animate={{ rotate: open ? 0 : -90 }} transition={transition}><ChevronDown /></motion.span>
    </button>
    <motion.div id={id} className="shNavGroupContent" initial={false} animate={{ height: open ? "auto" : 0, opacity: open ? 1 : 0 }} transition={transition} aria-hidden={!open} inert={!open}>
      <div className="shNavGroupRows">{children}</div>
    </motion.div>
  </>;
}
