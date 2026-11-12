import { useTranslation } from "../i18n";
import { ConversationDockContext, ConversationDockItem } from "../shell/ConversationDock";
import { useContext, useEffect, useState } from "react";
import { Link, useLocation, useNavigate, useRouteLoaderData, useSearchParams } from "react-router";
import { ArrowUpRight, CheckCircle2, ChevronRight, Clock3, FileText, Folder, ListFilter, MessageSquare, Minus, Plus, Star, SignalHigh, SignalMedium, SignalLow, RotateCcw, X } from "lucide-react";
import { useModalDialog } from "../components/useModalDialog";
import { displayWorkText, workText, exampleProjects, useWorkPrototype, workStatuses } from "./workPrototype";
import { ChoiceMenu, Property, StatusIcon } from "./WorkControls";
import "./work-prototype.css";

function useWorkCopy() {
  const { t } = useTranslation();
  const text = value => displayWorkText(value, t);
  const statusOptions = workStatuses.map(value => ({ value, label: t(`work.status.${value}`), icon: <StatusIcon status={value} /> }));
  const priorityOptions = [["high", <SignalHigh />], ["medium", <SignalMedium />], ["low", <SignalLow />]].map(([value, icon]) => ({ value, label: t(`work.priority.${value}`), icon }));
  const projectOptions = [{ value: "", label: t("work.noProject"), icon: <Folder /> }, ...exampleProjects.map(item => ({ value: item.id, label: text(item.name), icon: <Folder /> }))];
  const ownerOptions = ["me", "chen_yue"].map(value => ({ value, label: t(`work.owner.${value}`), icon: <span className="workAvatar" aria-hidden="true">{t(`work.owner.${value}`).slice(0, 1)}</span> }));
  return { t, text, statusOptions, priorityOptions, projectOptions, ownerOptions };
}

function NewTask({ onClose, onCreate, project }) {
  const { t, text } = useWorkCopy();
  const ref = useModalDialog({ onClose });
  return <div className="workDialogBackdrop"><form ref={ref} className="workDialog" role="dialog" aria-modal="true" aria-labelledby="work-new-title" tabIndex={-1} onSubmit={event => {
    event.preventDefault(); const data = new FormData(event.currentTarget); const title = String(data.get("title")).trim();
    if (title) onCreate({ title, project: String(data.get("project")), description: String(data.get("description")).trim(), deliverable: "" });
  }}>
    <header><div className="workBreadcrumb"><Folder /><span>{t("work.workspace")}</span><ChevronRight /><h2 id="work-new-title">{t("work.newTask")}</h2></div><button className="workIconButton" type="button" aria-label={t("work.cancel")} onClick={onClose}><X /></button></header>
    <label className="workTitleField"><span className="workSrOnly">{t("work.taskName")}</span><input name="title" required maxLength={160} autoFocus placeholder={t("work.taskPlaceholder")} /></label>
    <label className="workDescriptionField"><span className="workSrOnly">{t("work.description")}</span><textarea name="description" rows={4} placeholder={t("work.descriptionPlaceholder")} /></label>
    <label>{t("work.project")}<select name="project" defaultValue={project || ""}><option value="">{t("work.noProjectMembership")}</option>{exampleProjects.map(item => <option key={item.id} value={item.id}>{text(item.name)}</option>)}</select></label>
    <footer><span>{t("work.createPreview")}</span><button className="workPrimary" type="submit">{t("work.createTask")}</button></footer>
  </form></div>;
}

export default function WorkPrototypeRoute() {
  const dock = useContext(ConversationDockContext);
  const { t, text, statusOptions, priorityOptions, projectOptions, ownerOptions } = useWorkCopy();
  const { workspace } = useRouteLoaderData("workspace");
  const state = useWorkPrototype();
  const location = useLocation();
  const navigate = useNavigate();
  const [params, setParams] = useSearchParams();
  const base = `/w/${encodeURIComponent(workspace.id)}`;
  const path = location.pathname.slice(`${base}/work`.length).split("/").filter(Boolean);
  const page = path[0] || "mine";
  const task = page === "tasks" && path[1] ? state.tasks.find(item => item.id === path[1]) : null;
  const project = page === "projects" && path[1] ? exampleProjects.find(item => item.id === path[1]) : null;
  const [creating, setCreating] = useState(false);
  const [notice, setNotice] = useState("");
  const [conversation, setConversation] = useState(null);
  function openConversation(item) { dock?.minimizeChat(); setConversation(item); }
  const [document, setDocument] = useState(null);
  useEffect(() => {
    if (!notice) return undefined;
    const timer = window.setTimeout(() => setNotice(""), 4200);
    return () => window.clearTimeout(timer);
  }, [notice]);
  const tab = params.get("tab") || (task ? "overview" : project ? "overview" : "active");
  const statusFilter = params.get("status") || "all";
  function changeParam(key, value) { setParams(previous => { const next = new URLSearchParams(previous); if(value) next.set(key, value); else next.delete(key); return next; }); }
  function update(change, event) { state.updateTask(task.id, change, event); setNotice(event || workText("work.updated")); }
  function tabs(items) { return <nav className="workTabs" aria-label={t("work.contentViews")}>{items.map(([id, name]) => <button key={id} type="button" aria-pressed={tab === id} onClick={() => changeParam("tab", id)}>{text(name)}</button>)}</nav>; }
  function rows(items) {
    return items.length ? <div className="workRows">{items.map(item => <Link className="workRow" key={item.id} to={`${base}/work/tasks/${item.id}`}>
      <StatusIcon status={item.status} /><span className="workId">{item.id}</span><span className="workRowTitle">{text(item.title)}{item.attention && item.status !== "done" ? <small>{t("work.needsAttention")}</small> : null}</span><span className="workRowProject"><Folder />{text(exampleProjects.find(p => p.id === item.project)?.name || t("work.standalone"))}</span><span className="workAvatar" aria-label={t("work.ownerLabel", { owner: t(`work.owner.${item.owner}`) })}>{t(`work.owner.${item.owner}`).slice(0, 1)}</span>
    </Link>)}</div> : <div className="workEmpty"><CheckCircle2 /><h3>{t("work.emptyTitle")}</h3><p>{t("work.emptyHint")}</p><button onClick={() => setCreating(true)}>{t("work.newTask")}</button></div>;
  }
  function grouped(items) {
    return ["in_progress", "todo", "done", "cancelled"].map(status => {
      const group = items.filter(item => item.status === status);
      return group.length ? <details className="workGroup" key={status} open><summary><ChevronRight className="workGroupChevron" /><StatusIcon status={status} /><span>{t(`work.status.${status}`)}</span><small>{group.length}</small></summary>{rows(group)}</details> : null;
    });
  }
  function discussions(items) { return items.length ? items.map((item, i) => <button className="workResource" key={i} onClick={() => openConversation(item)}><MessageSquare /><span>{text(item.title)}<small>{text(item.state)}</small></span><ArrowUpRight /></button>) : <p className="workEmptyText">{t("work.noConversations")}</p>; }
  let content;
  if ((page === "tasks" || page === "projects") && path[1] && !task && !project) content = <div className="workEmpty"><h2>{t("work.notFound")}</h2><Link to={`${base}/work`}>{t("work.backToWork")}</Link></div>;
  else if (task) {
    const taskProject = exampleProjects.find(item => item.id === task.project);
    function simulateRun() {
      update({ run: workText("work.run.simulated"), conversations: [...task.conversations, { title: workText("work.executionTitle"), state: workText("work.run.simulated"), text: workText("work.executionText") }] }, workText("work.event.simulated"));
    }
    const latestConversation = task.conversations.at(-1);
    content = <>
      <header className="workHeader">
        <div className="workBreadcrumb"><Link to={`${base}/work/tasks`}>{t("work.tasks")}</Link><ChevronRight /><span>{task.id}</span></div>
        <button className="workQuiet" onClick={() => {
          const item = { title: workText("work.discussionTitle"), state: workText("work.run.discussion"), text: workText("work.discussionText", { title: task.title }) };
          update({ conversations: [...task.conversations, item] }, workText("work.event.linkedDiscussion")); openConversation(item);
        }}><MessageSquare />{t("work.addSampleDiscussion")}</button>
      </header>
      <div className="workDetail">
        <div className="workDetailMain">
          <h1>{text(task.title)}</h1>
          <section className="workProse"><p>{text(task.description) || t("work.noDescription")}</p><h2>{t("work.deliverable")}</h2><p>{text(task.deliverable) || t("work.noDeliverable")}</p></section>
          <section className={`workExecution ${task.attention ? "needsAttention" : ""}`} aria-label={t("work.currentRun")}>
            <div className="workExecutionSummary"><span className="workExecutionIcon"><Clock3 /></span><div><h2>{text(task.run)}</h2><p>{(latestConversation ? text(latestConversation.text) : "") || t("work.executionHint")}</p></div>
              {latestConversation ? <button onClick={() => openConversation(latestConversation)}>{t("work.continueWork")}<ArrowUpRight /></button> : <button onClick={simulateRun}>{t("work.simulateRun")}</button>}
            </div>
            {task.attention ? <button className="workQuiet" onClick={() => update({ attention: false, run: workText("work.run.awaitingNext") }, workText("work.event.confirmed"))}>{t("work.confirmResult")}</button> : null}
            {task.conversations.length ? <details className="workLinkedConversations"><summary><ChevronRight />{t("work.linkedConversations")}<span>{task.conversations.length}</span></summary>{discussions(task.conversations)}</details> : null}
          </section>
          {task.checks.length ? <section className="workSection" aria-labelledby="work-checks-title">
            <div className="workSectionHeading"><h2 id="work-checks-title">{t("work.checkResults")}</h2><span>{t("work.passedCount", { passed: task.checks.filter(check => check.result === "passed").length, total: task.checks.length })}</span></div>
            <div className="workChecks">{task.checks.map(check => <article className="workCheck" key={check.id}>
              <div><StatusIcon status={check.result === "passed" ? "done" : "todo"} /><h3>{text(check.title)}</h3><span>{t(`work.check.${check.result}`)}</span></div>
              <button className="workEvidence" onClick={() => setDocument(check.evidence)}>{text(check.evidence)}</button>
              {check.issue ? <><p>{text(check.issue)}<span className={`workPill ${check.resolution === "resolved" ? "isResolved" : ""}`}>{t(`work.resolution.${check.resolution}`)}</span></p>
                {check.resolution !== "resolved" ? <button onClick={() => {
                  const supplied = check.resolution === "pending";
                  const checks = task.checks.map(item => item.id === check.id ? { ...item, resolution: supplied ? "review" : "resolved", result: supplied ? "failed" : "passed" } : item);
                  update({ checks, attention: supplied, run: supplied ? workText("work.run.awaitingReview") : workText("work.run.reviewed") }, supplied ? workText("work.event.supplied") : workText("work.event.reviewed"));
                }}>{check.resolution === "pending" ? t("work.supplyFile") : t("work.simulateReview")}</button> : null}</> : null}
            </article>)}</div>
          </section> : null}
          {taskProject?.materials.length ? <section className="workSection"><div className="workSectionHeading"><h2>{t("work.linkedMaterials")}</h2><span>{taskProject.materials.length}</span></div><div className="workResourceList">{taskProject.materials.map(name => <button key={name.key || name} className="workResource" onClick={() => setDocument(name)}><FileText /><span>{text(name)}</span><ArrowUpRight /></button>)}</div></section> : null}
          <section className="workSection"><div className="workSectionHeading"><h2>{t("work.activity")}</h2></div>
            {task.events.length ? <ol className="workTimeline" aria-label={t("work.activityLog")}>{task.events.map((event, index) => <li key={index}><span className="workActivityDot" />{text(event)}</li>)}</ol> : <p className="workMuted">{t("work.noActivity")}</p>}
            <form className="workComment" onSubmit={event => { event.preventDefault(); const text = String(new FormData(event.currentTarget).get("comment")).trim(); if(text) { update({}, workText("work.event.comment", { text })); event.currentTarget.reset(); } }}><label htmlFor="work-comment">{t("work.addDiscussion")}</label><textarea id="work-comment" name="comment" required rows={2} placeholder={t("work.commentPlaceholder")} /><button type="submit">{t("work.post")}</button></form>
          </section>
        </div>
        <aside className="workProperties"><h2>{t("work.properties")}</h2>
          <Property label={t("work.status")} value={task.status} options={statusOptions} onChange={value => update({ status: value }, workText("work.event.statusChanged", { value: workText(`work.status.${value}`) }))} />
          <Property label={t("work.owner")} value={task.owner} options={ownerOptions} onChange={value => update({ owner: value }, workText("work.event.ownerChanged", { value: workText(`work.owner.${value}`) }))} />
          <Property label={t("work.priority")} value={task.priority} options={priorityOptions} onChange={value => update({ priority: value }, workText("work.event.priorityChanged", { value: workText(`work.priority.${value}`) }))} />
          <Property label={t("work.project")} value={task.project} options={projectOptions} onChange={value => update({ project: value }, workText("work.event.projectChanged"))} />
          {task.label ? <div className="workPropertySection"><h2>{t("work.labels")}</h2><span className="workLabel"><span />{text(task.label)}</span></div> : null}
          <p className="workPropertyHelp">{t("work.ownerConfirmation")}</p>
        </aside>
      </div>
    </>;
  } else if (project) {
    const items = state.tasks.filter(item => item.project === project.id);
    content = <><header className="workHeader"><div className="workBreadcrumb"><Link to={`${base}/work/projects`}>{t("work.projects")}</Link><span>/</span><span>{text(project.name)}</span></div><button aria-label={state.favorites.includes(project.id) ? t("work.unfavoriteProject") : t("work.favoriteProject")} aria-pressed={state.favorites.includes(project.id)} onClick={() => state.toggleFavorite(project.id)}><Star /></button></header><div className="workProjectBody"><div className="workProjectTitle"><span className="workProjectMark">{project.icon}</span><div><h1>{text(project.name)}</h1><p>{text(project.description)}</p></div></div>{tabs([["overview", t("work.overview")], ["tasks", t("work.taskCount", { count: items.length })], ["materials", t("work.materials")], ["conversations", t("work.conversations")]])}
    {tab === "overview" ? <><section className="workProjectOverview"><div className="workProse"><h2>{t("work.projectGoal")}</h2><p>{text(project.outcome)}</p><h2>{t("work.linkedMaterials")}</h2>{project.materials.map(name => <button key={name.key || name} className="workResource" onClick={() => setDocument(name)}><FileText /><span>{text(name)}</span><ArrowUpRight /></button>)}</div><dl><dt>{t("work.owner")}</dt><dd>{t(`work.owner.${project.owner}`)}</dd><dt>{t("work.progress")}</dt><dd>{t("work.completedCount", { completed: items.filter(item => item.status === "done").length, total: items.length })}</dd><dt>{t("work.capabilities")}</dt><dd>{text(project.skill)}</dd></dl></section><div className="workSectionHeading"><h2>{t("work.activeTasks")}</h2><button onClick={() => setCreating(true)}><Plus />{t("work.newTask")}</button></div>{rows(items.filter(item => item.status === "in_progress"))}</> : null}
    {tab === "tasks" ? <><div className="workToolbar"><span>{t("work.itemCount", { count: items.length })}</span><button onClick={() => setCreating(true)}><Plus />{t("work.newTask")}</button></div>{grouped(items)}</> : null}
    {tab === "materials" ? <section className="workSection">{project.materials.map(name => <button key={name.key || name} className="workResource" onClick={() => setDocument(name)}><FileText /><span>{text(name)}<small>{t("work.sampleMaterial")}</small></span><ArrowUpRight /></button>)}<Link className="workTextLink" to={`${base}/library`}>{t("work.openLibrary")}<ArrowUpRight /></Link></section> : null}
    {tab === "conversations" ? <section className="workSection">{discussions(items.flatMap(item => item.conversations))}</section> : null}</div></>;
  } else if (page === "projects") content = <><header className="workHeader"><h1><Folder />{t("work.projects")}<span className="workCount">{exampleProjects.length}</span></h1></header><div className="workProjectList"><div className="workProjectColumns"><span>{t("work.name")}</span><span>{t("work.progressColumn")}</span><span>{t("work.owner")}</span></div>{exampleProjects.map(item => { const tasks = state.tasks.filter(task => task.project === item.id); return <Link to={`${base}/work/projects/${item.id}`} className="workProjectRow" key={item.id}><span className="workProjectMark">{item.icon}</span><div><h2>{text(item.name)}</h2><p>{text(item.description)}</p></div><span className="workProjectProgress"><span className="workProgressTrack"><span style={{ width: `${tasks.length ? tasks.filter(task => task.status === "done").length / tasks.length * 100 : 0}%` }} /></span><span>{tasks.filter(task => task.status === "done").length} / {tasks.length}</span></span><span className="workAvatar">{t(`work.owner.${item.owner}`).slice(0, 1)}</span></Link>; })}</div></>;
  else {
    let items = state.tasks.filter(item => page !== "mine" || item.owner === "me");
    if (page === "mine") items = items.filter(item => tab === "attention" ? item.attention && !["done", "cancelled"].includes(item.status) : tab === "completed" ? item.status === "done" : !["done", "cancelled"].includes(item.status));
    items = items.filter(item => (statusFilter === "all" || item.status === statusFilter));
    content = <><header className="workHeader"><h1>{page === "mine" ? t("work.myWork") : t("work.allTasks")}</h1><button className="workPrimary" onClick={() => setCreating(true)}><Plus />{t("work.newTask")}</button></header><div className="workListControls">{page === "mine" ? tabs([["active", t("work.active")], ["attention", t("work.needsAttention")], ["completed", t("work.status.done")]]) : <span className="workMuted">{t("work.workspaceTasks")}</span>}<div className="workToolbar"><ChoiceMenu label={t("work.filterStatus")} value={statusFilter} options={[{ value: "all", label: t("work.allStatuses"), icon: <ListFilter /> }, ...statusOptions]} onChange={value => changeParam("status", value)} className="workFilter" /></div></div><div className="workListBody">{items.length ? grouped(items) : rows([])}</div><div className="workListFoot">{t("work.itemCount", { count: items.length })}</div></>;
  }
  return <div className="workPrototype"><div className="workPrototypeNotice"><span className="workPreviewBadge"><span />{t("work.preview")}</span><span className="workNoticeDetail">{t("work.sampleNotice")}</span><button onClick={() => { state.reset(); setNotice(workText("work.resetDone")); navigate(`${base}/work`); }}><RotateCcw />{t("work.reset")}</button></div>{notice ? <div className="workToast" role="status"><CheckCircle2 />{text(notice)}<button aria-label={t("work.closeNotice")} onClick={() => setNotice("")}><X /></button></div> : null}{content}
    {creating ? <NewTask project={project?.id} onClose={() => setCreating(false)} onCreate={values => { const id = state.createTask(values); setCreating(false); navigate(`${base}/work/tasks/${id}`); setNotice(workText("work.created")); }} /> : null}
    {conversation ? <FloatingConversation key={conversation.title.key || conversation.title} conversation={conversation} onClose={() => setConversation(null)} /> : null}
    {document ? <Preview onClose={() => setDocument(null)} title={document}><p className="workMuted">{t("work.sampleDocument")}</p><h3>{t("work.quotedContent")}</h3><p>{t(document.key === "work.example.document.checklist" || document.key === "work.example.check.exampleEvidence" ? "work.excerpt.release" : document.key === "work.example.document.interviews" || document.key === "work.example.document.requirements" ? "work.excerpt.retrieval" : "work.excerpt.generic")}</p></Preview> : null}
  </div>;
}

function Preview({ title, onClose, children }) {
  const { t, text } = useWorkCopy();
  const ref = useModalDialog({ onClose });
  return <div className="workDialogBackdrop"><section ref={ref} className="workDialog workPreview" role="dialog" aria-modal="true" aria-labelledby="work-preview-title" tabIndex={-1}><header><h2 id="work-preview-title">{text(title)}</h2><button onClick={onClose}>{t("work.close")}</button></header>{children}</section></div>;
}

function FloatingConversation({ conversation, onClose }) {
  const dock = useContext(ConversationDockContext);
  const { t, text } = useWorkCopy();
  const [minimized, setMinimized] = useState(false);
  useEffect(() => { if (dock?.chatFloating) setMinimized(true); }, [dock?.chatFloating]);
  const [draft, setDraft] = useState("");
  const [messages, setMessages] = useState([]);
  return <>
    <ConversationDockItem><button className="workConversationTab" aria-pressed={!minimized && !dock?.chatFloating} onClick={() => { if (minimized || dock?.chatFloating) { dock?.minimizeChat(); setMinimized(false); } else setMinimized(true); }}><MessageSquare />{text(conversation.title)}</button></ConversationDockItem>
    {!minimized && !dock?.chatFloating ? <section className="workFloatingConversation" role="dialog" aria-label={text(conversation.title)} onKeyDown={event => { if(event.key === "Escape") { event.stopPropagation(); onClose(); } }}>
      <header><h2>{text(conversation.title)}</h2><button aria-label={t("work.minimizeSample")} onClick={() => setMinimized(true)}><Minus /></button><button autoFocus aria-label={t("work.closeSample")} onClick={onClose}><X /></button></header>
      <div className="workFloatingMessages"><p className="workMuted">{t("work.conversation.sampleState", { state: text(conversation.state) })}</p><h3>Agent</h3><p>{text(conversation.text)}</p>{messages.map((message,index) => <p className="workExampleReply" key={index}>{message}</p>)}</div>
      <form onSubmit={event => { event.preventDefault(); if(draft.trim()) { setMessages(items => [...items,draft.trim()]); setDraft(""); } }}><textarea aria-label={t("work.sampleMessage")} placeholder={t("work.messagePlaceholder")} value={draft} onChange={event => setDraft(event.target.value)} rows={2} /><footer><span>{t("work.interactionOnly")}</span><button type="submit" disabled={!draft.trim()}>{t("work.send")}</button></footer></form>
    </section> : null}
  </>;
}
