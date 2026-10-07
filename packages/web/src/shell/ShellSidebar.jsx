import { t } from "../i18n";
import { useTranslation } from "../i18n";
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Link, useLocation, useNavigate, useRouteLoaderData } from "react-router";
import {
  Check,
  ChevronDown,
  FileText,
  Folder,
  Home,
  Library,
  LockKeyhole,
  LogOut,
  MessageSquare,
  PanelLeft,
  Plus,
  Search,
  SlidersHorizontal,
  SquarePen,
  Trash2,
  Upload,
  X,
} from "lucide-react";
import { apiJson, apiResponse, clearCsrfToken } from "../api";
import { useModalDialog } from "../components/useModalDialog";
import { AgentMark } from "./AgentMark";
import { agentChatPath } from "../agent-chat/agentChatNavigation";
import { SearchOverlay } from "./SearchOverlay";
import { TrashPopover } from "./TrashPopover";

const WORKSPACE_ROLE_LABELS = () => ({ owner: t("workspaceChooserRoute.owner"), admin: t("invitationActivationRoute.administrator"), member: t("invitationActivationRoute.member") });
const NOTE_TEMPLATES = () => ([
  { name: t("shellSidebar.taskList"), description: t("shellSidebar.trackTasksStatusAndNextSteps"), preview: [t("shellSidebar.toDo"), t("shellSidebar.inProgress"), t("shellSidebar.completed")], markdown: t("shellSidebar.taskListFirstTask") },
  { name: t("shellSidebar.projectNotes"), description: t("shellSidebar.organizeGoalsProgressAndKeyDecisions"), preview: [t("shellSidebar.goals"), t("shellSidebar.progress"), t("shellSidebar.nextSteps")], markdown: t("shellSidebar.projectNotesGoalsProgressDecisions") },
  { name: t("shellSidebar.researchNotes"), description: t("shellSidebar.collectQuestionsSourcesAndInterimFindings"), preview: [t("shellSidebar.questions"), t("libraryRoute.source"), t("shellSidebar.findings")], markdown: t("shellSidebar.researchNotesQuestionsSourcesFindings") },
  { name: t("shellSidebar.meetingNotes"), description: t("shellSidebar.recordTopicsDecisionsAndActionItems"), preview: [t("shellSidebar.topics"), t("shellSidebar.findings"), t("shellSidebar.actionItems")], markdown: t("shellSidebar.meetingNotesTopicsDecisionsActionItems") },
  { name: t("shellSidebar.decisionRecord"), description: t("shellSidebar.keepTheContextReasoningAndFollowUpImplications"), preview: [t("shellSidebar.context"), t("shellSidebar.decision"), t("shellSidebar.reasoning")], markdown: t("shellSidebar.decisionRecordContextDecisionReasoningFollowUp") },
  { name: t("shellSidebar.weeklyPlan"), description: t("shellSidebar.planYourPrioritiesAndReviewProgressThroughoutTheWeek"), preview: [t("shellSidebar.thisWeekSPriorities"), t("shellSidebar.tasks"), t("shellSidebar.review")], markdown: t("shellSidebar.weeklyPlanThisWeekSPrioritiesTasksReview") },
  { name: t("shellSidebar.readingList"), description: t("shellSidebar.organizeWhatToReadAndWhatYouLearn"), preview: [t("shellSidebar.toRead"), t("shellSidebar.reading"), t("shellSidebar.completed")], markdown: t("shellSidebar.readingListToReadReadingNotes") },
  { name: t("shellSidebar.contentOutline"), description: t("shellSidebar.organizeContentByTopicStructureAndMaterials"), preview: [t("shellSidebar.topic"), t("shellSidebar.structure"), t("shellSidebar.resources")], markdown: t("shellSidebar.contentOutlineTopicStructureResources") },
]);

function WorkspaceHeader({ workspace, workspaces, user, logoutBusy, logoutError, onLogout, onCollapse, returnTo }) {
  const { t } = useTranslation();
  const detailsRef = useRef(null);
  const base = `/w/${encodeURIComponent(workspace.id)}`;

  useEffect(() => {
    const closeOutside = (event) => {
      if (detailsRef.current?.open && !detailsRef.current.contains(event.target)) {
        detailsRef.current.removeAttribute("open");
      }
    };
    const closeOnEscape = (event) => {
      if (event.key === "Escape") detailsRef.current?.removeAttribute("open");
    };
    window.addEventListener("pointerdown", closeOutside);
    window.addEventListener("keydown", closeOnEscape);
    return () => {
      window.removeEventListener("pointerdown", closeOutside);
      window.removeEventListener("keydown", closeOnEscape);
    };
  }, []);

  return (
    <div className="shWs">
      <details className="shWsMenu" ref={detailsRef}>
        <summary className="shWsButton" role="button" aria-label={t("shellSidebar.valueWorkspaceMenu", { value1: workspace.name })}>
          <span className="shWsAvatar" aria-hidden="true">{workspace.name.slice(0, 1)}</span>
          <span className="shWsName">{workspace.name}</span>
          <ChevronDown aria-hidden="true" />
        </summary>
        <nav className="shWsMenuPopover" aria-label={t("shellSidebar.valueWorkspaceActions", { value1: workspace.name })}>
          <div className="shWsMenuIdentity">
            <span className="shWsAvatar" aria-hidden="true">{workspace.name.slice(0, 1)}</span>
            <span><strong>{workspace.name}</strong><small>{WORKSPACE_ROLE_LABELS()[workspace.role]}</small></span>
          </div>
          <div className="shWsMenuDivider" />
          <div className="shWsMenuAccount">{user.email}</div>
          <Link to={`${base}/settings/preferences`} state={{ returnTo }}><SlidersHorizontal aria-hidden="true" />{t("shellSidebar.settings")}</Link>
          {workspaces.length > 1 ? <div className="shWsMenuWorkspaces" role="group" aria-label={t("shellSidebar.switchWorkspace")}>
            {workspaces.map((item) => item.id === workspace.id
              ? <span className="shWsMenuWorkspace isCurrent" aria-current="page" key={item.id}>
                  <span className="shWsAvatar" aria-hidden="true">{item.name.slice(0, 1)}</span>
                  <span>{item.name}</span>
                  <Check aria-hidden="true" />
                </span>
              : <Link className="shWsMenuWorkspace" to={`/w/${encodeURIComponent(item.id)}/app`} key={item.id}>
                  <span className="shWsAvatar" aria-hidden="true">{item.name.slice(0, 1)}</span>
                  <span>{item.name}</span>
                </Link>)}
          </div> : null}
          <div className="shWsMenuDivider" />
          <button type="button" disabled={logoutBusy} onClick={onLogout}><LogOut aria-hidden="true" />{logoutBusy ? t("shellSidebar.signingOut") : t("shellSidebar.signOut")}</button>
          {logoutError ? <p className="shWsMenuError" role="alert">{logoutError}</p> : null}
        </nav>
      </details>
      {onCollapse ? <button className="shSidebarCollapse" type="button" aria-label={t("shellSidebar.hideSidebar")} title={t("shellSidebar.hideSidebar")} onClick={onCollapse}><PanelLeft aria-hidden="true" /></button> : null}
    </div>
  );
}

function PrivateCreateDialog({ onClose, onCreateNote, onUpload }) {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const dialogRef = useModalDialog({ onClose });
  const normalizedQuery = query.trim().toLocaleLowerCase();
  const visibleTemplates = NOTE_TEMPLATES().filter((template) => !normalizedQuery || `${template.name}${template.description}`.toLocaleLowerCase().includes(normalizedQuery));

  return createPortal(<div className="shPrivateCreateBackdrop" role="presentation" onMouseDown={onClose}>
    <section className="shPrivateCreateDialog" ref={dialogRef} role="dialog" aria-modal="true" aria-label={t("shellSidebar.addPrivateContent")} tabIndex={-1} onMouseDown={(event) => event.stopPropagation()}>
      <header className="shPrivateCreateHeader">
        <div><button className="quietCloseButton" type="button" aria-label={t("shellSidebar.closeAddMenu")} onClick={onClose}><X aria-hidden="true" /></button><span>{t("libraryRoute.addTo")}</span><strong><LockKeyhole aria-hidden="true" />{t("agentRoute.private")}</strong></div>
        <label><Search aria-hidden="true" /><input autoFocus aria-label={t("shellSidebar.searchTemplates")} placeholder={t("libraryRoute.search")} value={query} onChange={(event) => setQuery(event.target.value)} /></label>
      </header>
      <div className="shPrivateCreateScroll">
        <div className="shPrivateCreateContent">
          <div className="shPrivateCreateQuick">
            <button type="button" onClick={() => onCreateNote({ name: "Untitled", markdown: "" })}><FileText aria-hidden="true" /><strong>{t("shellSidebar.blankNote")}</strong></button>
            <button type="button" onClick={onUpload}><Upload aria-hidden="true" /><strong>{t("shellSidebar.uploadMaterials")}</strong></button>
          </div>
          <h2><FileText aria-hidden="true" />{t("shellSidebar.templates")}</h2>
          <div className="shPrivateTemplateGrid">
            {visibleTemplates.map((template) => <button className="shPrivateTemplateCard" type="button" key={template.name} onClick={() => onCreateNote(template)}>
              <span><strong>{template.name}</strong><small>{template.description}</small></span>
              <div className="shPrivateTemplatePreview" aria-hidden="true"><strong>{template.name}</strong>{template.preview.map((label) => <i key={label}><span /><em>{label}</em></i>)}</div>
            </button>)}
            {!visibleTemplates.length ? <p>{t("shellSidebar.noMatchingTemplates")}</p> : null}
          </div>
        </div>
      </div>
    </section>
  </div>, document.body);
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

function HomeTab({ base, notes, notesError, onCreateNote, onOpenCreate, trashOpen, onToggleTrash, trashTriggerRef }) {
  const { t } = useTranslation();
  return (
    <div className="shScroll">
      <section className="shSection shCollapsibleSection">
        <details className="shDisclosure" open>
          <summary className="shSectionHeader shDisclosureSummary"><span>{t("agentRoute.private")}</span><ChevronDown aria-hidden="true" /></summary>
          <div className="shDisclosureBody">
            {(notes || []).map((note) => <Link className="shRow" to={`${base}/library/${encodeURIComponent(note.id)}`} key={note.id}><FileText aria-hidden="true" /><span>{note.displayName}</span></Link>)}
            {notesError ? <p className="shEmptyHint">{t("shellSidebar.unableToLoadPrivateDocuments")}</p> : null}
            {notes && notes.length <= 2 ? <button className="shRow" type="button" onClick={onOpenCreate}><Plus aria-hidden="true" /><span>{t("shellSidebar.addNew")}</span></button> : null}
          </div>
        </details>
        <button className="shSectionAction" type="button" aria-label={t("shellSidebar.addToPrivate")} title={t("shellSidebar.newPage")} onClick={onCreateNote}><Plus aria-hidden="true" /></button>
      </section>

      <section className="shSection shPrimaryNav">
        <Link className="shRow" to={`${base}/library`}><Library aria-hidden="true" /><span>{t("workspaceContextPanel.library")}</span></Link>
        <button className={`shRow ${trashOpen ? "isActive" : ""}`} ref={trashTriggerRef} type="button" data-trash-trigger aria-expanded={trashOpen} onClick={onToggleTrash}><Trash2 aria-hidden="true" /><span>{t("shellSidebar.trash")}</span></button>
      </section>
    </div>
  );
}

function ConversationTab({ agents, activeAgent, base, workspaceId, sessionProps, recentNavigation, onStartNewChat, sidebarTab = "chat" }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const location = useLocation();
  const query = new URLSearchParams(location.search);
  const startNewChat = (projectId = "") => {
    if (onStartNewChat) onStartNewChat(projectId, sidebarTab);
    else {
      const query = new URLSearchParams({ new: "1" });
      if (projectId) query.set("projectId", projectId);
      navigate(activeAgent ? `${base}/agents/${encodeURIComponent(activeAgent.id)}?${query}` : `${base}/app`, { state: { sidebarTab } });
    }
  };
  return (
    <div className="shScroll">
      <section className="shSection">
        <header className="shSectionHeader">
          <span>{t("agentRunRow.agent")}</span>
        </header>
        <div className="shAgentStrip">
          {agents.map((agent) => {
            const target = agentChatPath(workspaceId, agent.id);
            const selected = location.pathname === target && !query.has("sessionId") && !query.has("new");
            return <Link className={`shRow ${selected ? "isActive" : ""}`} aria-current={selected ? "page" : undefined} to={target} state={{ sidebarTab }} key={agent.id}><AgentMark className="shAgentGlyph" agent={agent} /><span>{agent.name}</span></Link>;
          })}
        </div>
      </section>

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
          <button className="shSectionAction" type="button" aria-label={t("shellSidebar.newGeneralConversation")} onClick={() => startNewChat()}><Plus aria-hidden="true" /></button>
        </section>
      </> : null}
      {recentNavigation?.loading ? <p className="shEmptyHint" role="status">{t("agentChat.loading")}</p> : null}
      {recentNavigation?.error ? <p className="shEmptyHint" role="alert">{t("agentChat.recentError")} <button type="button" onClick={recentNavigation.onRetry}>{t("agentChat.retry")}</button></p> : null}
    </div>
  );
}

export function ShellSidebar({ workspace, agents, activeAgent, sessionProps, recentNavigation, onStartNewChat, onCollapse, initialTab = "home" }) {
  const { t } = useTranslation();
  const [tab, setTab] = useState(initialTab);
  const [searchOpen, setSearchOpen] = useState(false);
  const [logoutBusy, setLogoutBusy] = useState(false);
  const [logoutError, setLogoutError] = useState("");
  const [privateCreateOpen, setPrivateCreateOpen] = useState(false);
  const [projectCreateOpen, setProjectCreateOpen] = useState(false);
  const [privateNotes, setPrivateNotes] = useState(null);
  const [privateNotesError, setPrivateNotesError] = useState(false);
  const [trashOpen, setTrashOpen] = useState(false);
  const trashTriggerRef = useRef(null);
  const { user } = useRouteLoaderData("authenticated");
  const { workspaces = [workspace] } = useRouteLoaderData("workspace");
  const location = useLocation();
  const navigate = useNavigate();
  const lastInitialTab = useRef(initialTab);
  const base = `/w/${encodeURIComponent(workspace.id)}`;

  useEffect(() => {
    if (lastInitialTab.current !== initialTab) {
      lastInitialTab.current = initialTab;
      setTab(initialTab);
    }
  }, [initialTab]);

  useEffect(() => {
    let active = true;
    setPrivateNotesError(false);
    apiJson("/api/library")
      .then((result) => {
        if (!active) return;
        if (!Array.isArray(result.objects)) throw new Error("library_objects_invalid");
        setPrivateNotes(result.objects.filter((item) => item.objectKind === "note"));
      })
      .catch(() => active && setPrivateNotesError(true));
    return () => { active = false; };
  }, []);

  useEffect(() => {
    const onKeyDown = (event) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setSearchOpen(true);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  function showHome() {
    setTrashOpen(false);
    setTab("home");
    if (!sessionProps && location.pathname !== `${base}/app`) navigate(`${base}/app`, { state: { sidebarTab: "home" } });
  }

  function showConversations() {
    setTrashOpen(false);
    setTab("chat");
    if (sessionProps) return;
    if (activeAgent) navigate(`${base}/agents/${encodeURIComponent(activeAgent.id)}`);
    else if (agents.length === 1) navigate(`${base}/agents/${encodeURIComponent(agents[0].id)}`);
    else navigate(`${base}/app`);
  }

  const defaultNewChat = () => {
    navigate(activeAgent ? `${base}/agents/${encodeURIComponent(activeAgent.id)}?new=1` : `${base}/app`, { state: { sidebarTab: tab } });
  };

  function createNote(template) {
    setPrivateCreateOpen(false);
    navigate(`${base}/library/new`, { state: { noteDraft: template } });
  }

  function openUpload() {
    setPrivateCreateOpen(false);
    navigate(`${base}/library?upload=1`);
  }

  async function logout() {
    if (logoutBusy) return;
    setLogoutBusy(true);
    setLogoutError("");
    try {
      await apiResponse("/api/logout", { method: "POST" });
      clearCsrfToken();
      navigate("/login", { replace: true });
    } catch {
      setLogoutError(t("shellSidebar.unableToSignOutPleaseTryAgain"));
      setLogoutBusy(false);
    }
  }

  return (
    <aside className="workspaceSidebar shSidebar" aria-label={t("shellSidebar.conversationNavigation")}>
      <WorkspaceHeader workspace={workspace} workspaces={workspaces} user={user} logoutBusy={logoutBusy} logoutError={logoutError} onLogout={logout} onCollapse={onCollapse ? () => { setTrashOpen(false); onCollapse(); } : undefined} returnTo={`${location.pathname}${location.search}`} />
      <div className="shSidebarNewChat">
        <button className="shNewChat" type="button" onClick={() => onStartNewChat ? onStartNewChat("", tab) : defaultNewChat()}><SquarePen aria-hidden="true" />{t("shellSidebar.newConversation")}</button>
      </div>
      <div className="shTabs" role="tablist" aria-label={t("shellSidebar.mainView")}>
        <button className={`shTab ${tab === "home" ? "isActive" : ""}`} type="button" role="tab" aria-label={t("homePlane.home")} title={t("homePlane.home")} aria-selected={tab === "home"} onClick={showHome}>
          <Home aria-hidden="true" />
        </button>
        <button className={`shTab ${tab === "chat" ? "isActive" : ""}`} type="button" role="tab" aria-label={t("shellSidebar.chat")} title={t("shellSidebar.chat")} aria-selected={tab === "chat"} onClick={showConversations}>
          <MessageSquare aria-hidden="true" />
        </button>
        <button className="shSearchButton" type="button" aria-label={t("searchOverlay.searchConversationsAndNotes")} title={t("shellSidebar.searchCtrlK")} onClick={() => setSearchOpen(true)}>
          <Search aria-hidden="true" />
        </button>
      </div>

      {tab === "home" ? <HomeTab base={base} notes={privateNotes} notesError={privateNotesError} onCreateNote={() => { setTrashOpen(false); createNote({ name: "Untitled", markdown: "" }); }} onOpenCreate={() => { setTrashOpen(false); setPrivateCreateOpen(true); }} trashOpen={trashOpen} onToggleTrash={() => { setTrashOpen((value) => !value); }} trashTriggerRef={trashTriggerRef} /> : <ConversationTab agents={agents} activeAgent={activeAgent} base={base} workspaceId={workspace.id} sessionProps={sessionProps ? { ...sessionProps, onOpenProjectCreate: () => setProjectCreateOpen(true) } : null} recentNavigation={recentNavigation} onStartNewChat={onStartNewChat} sidebarTab={tab} />}

      {searchOpen ? <SearchOverlay sessions={sessionProps?.sessions || []} workspace={workspace} agents={agents} agentId={sessionProps?.agentId || activeAgent?.id || ""} onClose={() => setSearchOpen(false)} /> : null}
      {privateCreateOpen ? <PrivateCreateDialog onClose={() => setPrivateCreateOpen(false)} onCreateNote={createNote} onUpload={openUpload} /> : null}
      {projectCreateOpen ? <ProjectCreateDialog onClose={() => setProjectCreateOpen(false)} onCreate={sessionProps.onCreateProject} /> : null}
      <TrashPopover open={trashOpen} workspace={workspace} anchorRef={trashTriggerRef} onClose={() => setTrashOpen(false)} />
    </aside>
  );
}
