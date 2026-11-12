import { useEffect, useState } from "react";
import { Link, useLocation, useNavigate, useRouteLoaderData, useSearchParams } from "react-router";
import { ArrowLeft, ArrowUpRight, CheckCircle2, ChevronRight, Clock3, FileText, Folder, ListFilter, MessageSquare, Plus, Search, Star, SignalHigh, SignalMedium, SignalLow, RotateCcw, X } from "lucide-react";
import { useModalDialog } from "../components/useModalDialog";
import { exampleProjects, useWorkPrototype, workStatuses } from "./workPrototype";
import { ChoiceMenu, Property, StatusIcon } from "./WorkControls";
import "./work-prototype.css";

const statusOptions = workStatuses.map(value => ({ value, label: value, icon: <StatusIcon status={value} /> }));
const priorityOptions = [{ value: "高", label: "高", icon: <SignalHigh /> }, { value: "中", label: "中", icon: <SignalMedium /> }, { value: "低", label: "低", icon: <SignalLow /> }];
const projectOptions = [{ value: "", label: "无项目", icon: <Folder /> }, ...exampleProjects.map(item => ({ value: item.id, label: item.name, icon: <Folder /> }))];
const ownerOptions = ["我", "陈悦"].map(value => ({ value, label: value, icon: <span className="workAvatar" aria-hidden="true">{value[0]}</span> }));

function NewTask({ onClose, onCreate, project }) {
  const ref = useModalDialog({ onClose });
  return <div className="workDialogBackdrop"><form ref={ref} className="workDialog" role="dialog" aria-modal="true" aria-labelledby="work-new-title" tabIndex={-1} onSubmit={event => {
    event.preventDefault(); const data = new FormData(event.currentTarget); const title = String(data.get("title")).trim();
    if (title) onCreate({ title, project: String(data.get("project")), description: String(data.get("description")).trim(), deliverable: "" });
  }}>
    <header><div className="workBreadcrumb"><Folder /><span>工作区</span><ChevronRight /><h2 id="work-new-title">新建任务</h2></div><button className="workIconButton" type="button" aria-label="取消" onClick={onClose}><X /></button></header>
    <label className="workTitleField"><span className="workSrOnly">任务名称</span><input name="title" required maxLength={160} autoFocus placeholder="需要完成什么？" /></label>
    <label className="workDescriptionField"><span className="workSrOnly">说明</span><textarea name="description" rows={4} placeholder="补充背景和期望结果…" /></label>
    <label>项目<select name="project" defaultValue={project || ""}><option value="">不属于项目</option>{exampleProjects.map(item => <option key={item.id} value={item.id}>{item.name}</option>)}</select></label>
    <footer><span>交互预览 · 仅创建示例任务</span><button className="workPrimary" type="submit">创建任务</button></footer>
  </form></div>;
}

export default function WorkPrototypeRoute() {
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
  const [document, setDocument] = useState(null);
  useEffect(() => {
    if (!notice) return undefined;
    const timer = window.setTimeout(() => setNotice(""), 4200);
    return () => window.clearTimeout(timer);
  }, [notice]);
  const tab = params.get("tab") || (task ? "overview" : project ? "overview" : "active");
  const query = params.get("q") || "";
  const statusFilter = params.get("status") || "all";
  function changeParam(key, value) { setParams(previous => { const next = new URLSearchParams(previous); if(value) next.set(key, value); else next.delete(key); return next; }); }
  function update(change, event) { state.updateTask(task.id, change, event); setNotice(event || "任务已更新"); }
  function tabs(items) { return <nav className="workTabs" aria-label="内容视图">{items.map(([id, name]) => <button key={id} type="button" aria-pressed={tab === id} onClick={() => changeParam("tab", id)}>{name}</button>)}</nav>; }
  function rows(items) {
    return items.length ? <div className="workRows">{items.map(item => <Link className="workRow" key={item.id} to={`${base}/work/tasks/${item.id}`}>
      <StatusIcon status={item.status} /><span className="workId">{item.id}</span><span className="workRowTitle">{item.title}{item.attention && item.status !== "已完成" ? <small>需要回应</small> : null}</span><span className="workRowLabel">{item.label}</span><span className="workRowProject"><Folder />{exampleProjects.find(p => p.id === item.project)?.name || "独立任务"}</span><span className="workRowPriority" aria-label={`优先级：${item.priority}`}>{priorityOptions.find(option => option.value === item.priority)?.icon}</span><span className="workAvatar" aria-label={`负责人：${item.owner}`}>{item.owner.slice(0, 1)}</span>
    </Link>)}</div> : <div className="workEmpty"><CheckCircle2 /><h3>这里暂时没有任务</h3><p>调整筛选条件，或创建一项新任务。</p><button onClick={() => setCreating(true)}>新建任务</button></div>;
  }
  function grouped(items) {
    return ["进行中", "待开始", "已完成", "已取消"].map(status => {
      const group = items.filter(item => item.status === status);
      return group.length ? <details className="workGroup" key={status} open><summary><ChevronRight className="workGroupChevron" /><StatusIcon status={status} /><span>{status}</span><small>{group.length}</small></summary>{rows(group)}</details> : null;
    });
  }
  function discussions(items) { return items.length ? items.map((item, i) => <button className="workResource" key={`${item.title}-${i}`} onClick={() => setConversation(item)}><MessageSquare /><span>{item.title}<small>{item.state}</small></span><ArrowUpRight /></button>) : <p className="workEmptyText">还没有关联会话。可以先讨论，再将结果关联到任务。</p>; }
  let content;
  if ((page === "tasks" || page === "projects") && path[1] && !task && !project) content = <div className="workEmpty"><h2>找不到这项示例内容</h2><Link to={`${base}/work`}>返回我的工作</Link></div>;
  else if (task) {
    const taskProject = exampleProjects.find(item => item.id === task.project);
    content = <>
      <header className="workHeader"><div className="workBreadcrumb"><Link to={`${base}/work/tasks`}><ArrowLeft />任务</Link><span>/</span><span>{task.id}</span></div><button onClick={() => { update({ conversations: [...task.conversations, { title: "任务讨论", state: "示例讨论", text: `围绕「${task.title}」确认目标、输入材料与交付要求。` }] }, "关联了一段示例讨论。"); changeParam("tab", "conversations"); }}>添加示例讨论</button></header>
      <div className="workDetail"><div className="workDetailMain"><h1>{task.title}</h1><div className="workTaskContext">{taskProject ? <Link to={`${base}/work/projects/${taskProject.id}`}><Folder />{taskProject.name}</Link> : <span>独立任务</span>}{task.label ? <span>{task.label}</span> : null}</div>
      {tabs([["overview", "概览"], ["conversations", `会话 ${task.conversations.length}`], ...(task.checks.length ? [["checks", "核查结果"]] : []), ["activity", "活动"]])}
      {tab === "overview" ? <><section className="workProse"><h2>说明</h2><p>{task.description || "暂未填写说明。"}</p><h2>交付要求</h2><p>{task.deliverable || "尚未指定交付要求。"}</p></section><section className="workSection"><div className="workSectionHeading"><h2>关联内容</h2><span>示例资料</span></div>{(taskProject?.materials || []).map(name => <button key={name} className="workResource" onClick={() => setDocument(name)}><FileText /><span>{name}</span><ArrowUpRight /></button>)}</section><section className="workSection"><h2>最近执行</h2><div className="workExecution"><span><Clock3 />{task.run}</span><p>查看执行记录，或继续处理当前任务。</p>{task.attention ? <button onClick={() => { update({ attention: false, run: "等待下一次执行" }, "已确认当前结果，任务继续进行。"); }}>确认当前结果</button> : <button onClick={() => { update({ run: "示例执行已完成", conversations: [...task.conversations, { title: "任务执行记录", state: "示例执行已完成", text: "已根据当前任务说明整理出初步结果，请负责人检查交付要求。" }] }, "模拟执行完成，业务状态保持不变。"); }}>模拟一次执行</button>}</div></section></> : null}
      {tab === "conversations" ? <section className="workSection">{discussions(task.conversations)}<Link className="workTextLink" to={`${base}/app`}>打开真实会话入口 <ArrowUpRight /></Link></section> : null}
      {tab === "checks" ? <section className="workSection"><div className="workSectionHeading"><h2>材料核查</h2><span>{task.checks.filter(check => check.result === "通过").length} / {task.checks.length} 项通过</span></div><p className="workMuted">按发布清单核查 · 本轮 3 项要求</p>{task.checks.map(check => <article className="workCheck" key={check.id}><div><StatusIcon status={check.result === "通过" ? "已完成" : "待开始"} /><h3>{check.title}</h3><span>{check.result}</span></div><button className="workEvidence" onClick={() => setDocument(check.evidence)}>{check.evidence}</button>{check.issue ? <><p>{check.issue} <span className={`workPill ${check.resolution === "已解决" ? "isResolved" : ""}`}>{check.resolution}</span></p>{check.resolution !== "已解决" ? <button onClick={() => { const supplied = check.resolution === "待处理"; const checks = task.checks.map(item => item.id === check.id ? { ...item, resolution: supplied ? "待复核" : "已解决", result: supplied ? "未通过" : "通过" } : item); update({ checks, attention: supplied, run: supplied ? "等待复核" : "核查已完成" }, supplied ? "补充了示例文件 quick-start.md。" : "示例复核通过，任务等待负责人确认。"); }}>{check.resolution === "待处理" ? "补充示例文件" : "模拟复核"}</button> : null}</> : null}</article>)}</section> : null}
      {tab === "activity" ? <section className="workSection"><h2>活动记录</h2>{task.events.length ? <ol className="workTimeline">{task.events.map((event, index) => <li key={`${index}-${event}`}>{event}</li>)}</ol> : <p className="workMuted">暂无活动记录。</p>}<form className="workComment" onSubmit={event => { event.preventDefault(); const text = String(new FormData(event.currentTarget).get("comment")).trim(); if(text) { update({}, `我：${text}`); event.currentTarget.reset(); } }}><label htmlFor="work-comment">添加讨论</label><textarea id="work-comment" name="comment" required rows={3} placeholder="记录补充信息或下一步…" /><button type="submit">发表</button></form></section> : null}
      </div><aside className="workProperties"><h2>属性</h2>
        <Property label="状态" value={task.status} options={statusOptions} onChange={value => update({ status: value }, `状态变更为${value}。`)} />
        <Property label="负责人" value={task.owner} options={ownerOptions} onChange={value => update({ owner: value }, `负责人变更为${value}。`)} />
        <Property label="优先级" value={task.priority} options={priorityOptions} onChange={value => update({ priority: value }, `优先级变更为${value}。`)} />
        <div className="workPropertySection"><h2>项目</h2><ChoiceMenu label="项目" value={task.project} options={projectOptions} onChange={value => update({ project: value }, "更新了所属项目。")} /></div>
        {task.label ? <div className="workPropertySection"><h2>标签</h2><span className="workLabel"><span />{task.label}</span></div> : null}
        <div className="workPropertySection"><h2>Agent 执行</h2><p className="workRunState"><Clock3 />{task.run}</p><p className="workPropertyHelp">任务进度由负责人确认。</p></div>
      </aside></div>
    </>;
  } else if (project) {
    const items = state.tasks.filter(item => item.project === project.id);
    content = <><header className="workHeader"><div className="workBreadcrumb"><Link to={`${base}/work/projects`}>项目</Link><span>/</span><span>{project.name}</span></div><button aria-label={state.favorites.includes(project.id) ? "取消收藏项目" : "收藏项目"} aria-pressed={state.favorites.includes(project.id)} onClick={() => state.toggleFavorite(project.id)}><Star /></button></header><div className="workProjectBody"><div className="workProjectTitle"><span className="workProjectMark">{project.icon}</span><div><h1>{project.name}</h1><p>{project.description}</p></div></div>{tabs([["overview", "概览"], ["tasks", `任务 ${items.length}`], ["materials", "资料"], ["conversations", "会话"]])}
    {tab === "overview" ? <><section className="workProjectOverview"><div className="workProse"><h2>项目目标</h2><p>{project.outcome}</p><h2>关联资料</h2>{project.materials.map(name => <button key={name} className="workResource" onClick={() => setDocument(name)}><FileText /><span>{name}</span><ArrowUpRight /></button>)}</div><dl><dt>负责人</dt><dd>{project.owner}</dd><dt>进展</dt><dd>{items.filter(item => item.status === "已完成").length} / {items.length} 项完成</dd><dt>可用能力</dt><dd>{project.skill}</dd></dl></section><div className="workSectionHeading"><h2>进行中的任务</h2><button onClick={() => setCreating(true)}><Plus />新建任务</button></div>{rows(items.filter(item => item.status === "进行中"))}</> : null}
    {tab === "tasks" ? <><div className="workToolbar"><span>{items.length} 项任务</span><button onClick={() => setCreating(true)}><Plus />新建任务</button></div>{grouped(items)}</> : null}
    {tab === "materials" ? <section className="workSection">{project.materials.map(name => <button key={name} className="workResource" onClick={() => setDocument(name)}><FileText /><span>{name}<small>示例资料 · 本项目关联</small></span><ArrowUpRight /></button>)}<Link className="workTextLink" to={`${base}/library`}>打开真实资料库 <ArrowUpRight /></Link></section> : null}
    {tab === "conversations" ? <section className="workSection">{discussions(items.flatMap(item => item.conversations))}</section> : null}</div></>;
  } else if (page === "projects") content = <><header className="workHeader"><h1><Folder />项目<span className="workCount">{exampleProjects.length}</span></h1></header><div className="workProjectList"><div className="workProjectColumns"><span>名称</span><span>进度</span><span>负责人</span></div>{exampleProjects.map(item => { const tasks = state.tasks.filter(task => task.project === item.id); return <Link to={`${base}/work/projects/${item.id}`} className="workProjectRow" key={item.id}><span className="workProjectMark">{item.icon}</span><div><h2>{item.name}</h2><p>{item.description}</p></div><span className="workProjectProgress"><span className="workProgressTrack"><span style={{ width: `${tasks.length ? tasks.filter(task => task.status === "已完成").length / tasks.length * 100 : 0}%` }} /></span><span>{tasks.filter(task => task.status === "已完成").length} / {tasks.length}</span></span><span className="workAvatar">{item.owner[0]}</span></Link>; })}</div></>;
  else {
    let items = state.tasks.filter(item => page !== "mine" || item.owner === "我");
    if (page === "mine") items = items.filter(item => tab === "attention" ? item.attention && !["已完成", "已取消"].includes(item.status) : tab === "completed" ? item.status === "已完成" : !["已完成", "已取消"].includes(item.status));
    items = items.filter(item => (statusFilter === "all" || item.status === statusFilter) && `${item.id} ${item.title}`.toLowerCase().includes(query.toLowerCase()));
    content = <><header className="workHeader"><h1>{page === "mine" ? "我的工作" : "所有任务"}</h1><button className="workPrimary" onClick={() => setCreating(true)}><Plus />新建任务</button></header>{page === "mine" ? tabs([["active", "待推进"], ["attention", "需要回应"], ["completed", "已完成"]]) : null}<div className="workToolbar"><label className="workSearch"><Search /><input type="search" aria-label="筛选任务" placeholder="筛选任务…" value={query} onChange={event => changeParam("q", event.target.value)} /></label><ChoiceMenu label="筛选状态" value={statusFilter} options={[{ value: "all", label: "所有状态", icon: <ListFilter /> }, ...statusOptions]} onChange={value => changeParam("status", value)} className="workFilter" /></div>{items.length ? grouped(items) : rows([])}<div className="workListFoot">{items.length} 项任务</div></>;
  }
  return <div className="workPrototype"><div className="workPrototypeNotice"><span className="workPreviewBadge"><span />交互预览</span><span className="workNoticeDetail">示例数据 · 刷新后重置</span><button onClick={() => { state.reset(); setNotice("示例已重置"); navigate(`${base}/work`); }}><RotateCcw />重置示例</button></div>{notice ? <div className="workToast" role="status"><CheckCircle2 />{notice}<button aria-label="关闭提示" onClick={() => setNotice("")}><X /></button></div> : null}{content}
    {creating ? <NewTask project={project?.id} onClose={() => setCreating(false)} onCreate={values => { const id = state.createTask(values); setCreating(false); navigate(`${base}/work/tasks/${id}`); setNotice("示例任务已创建"); }} /> : null}
    {conversation || document ? <Preview onClose={() => { setConversation(null); setDocument(null); }} title={conversation?.title || document}>{conversation ? <><p className="workMuted">{conversation.state} · 示例会话</p><h3>Agent</h3><p>{conversation.text}</p><Link className="workTextLink" to={`${base}/app`} onClick={() => setConversation(null)}>新建真实会话 <ArrowUpRight /></Link></> : <><p className="workMuted">示例资料，不代表真实文件内容</p><h3>引用内容</h3><p>{String(document).includes("清单") ? "发布材料应包含使用说明、许可说明和可运行的快速开始示例。" : String(document).includes("检索") ? "用户应当能在有权访问的资料中找到依据，并通过引用定位回到原文。" : "此处展示任务或项目关联的资料。正式实现将使用现有资料阅读与引用能力。"}</p></>}</Preview> : null}
  </div>;
}

function Preview({ title, onClose, children }) {
  const ref = useModalDialog({ onClose });
  return <div className="workDialogBackdrop"><section ref={ref} className="workDialog workPreview" role="dialog" aria-modal="true" aria-labelledby="work-preview-title" tabIndex={-1}><header><h2 id="work-preview-title">{title}</h2><button onClick={onClose}>关闭</button></header>{children}</section></div>;
}
