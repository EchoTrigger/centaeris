import { useEffect, useRef, useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";
import { Button } from "@centaeris/ui/button";
import { Archive, ChevronRight, Ellipsis, Pencil, Plus, UserRound } from "lucide-react";
import { Menu } from "@centaeris/ui/primitives/menu";
import { apiJson, jsonOptions } from "../api";
import { useTranslation } from "../i18n";
import { WorkBreadcrumb } from "./WorkBreadcrumb";
import { IssueForm, IssueStatus, issueStatuses } from "./IssueForm";
import { ChoiceMenu } from "./WorkControls";
import { IssueMaterials } from "./IssueMaterials";
import { IssueExecutions } from "./IssueExecutions";
import { IssueTemplateValues } from "./IssueTemplateFields";
import { IssueDeliverables } from "./IssueDeliverables";
import "./issues.css";

const pageSize = 50;
export function Issues({ workspaceId, userId, mine, issueId }) {
  const { t, i18n } = useTranslation();
  const navigate = useNavigate();
  const [templates, setTemplates] = useState([]);
  const [params, setParams] = useSearchParams();
  const archived = params.get("archived") === "true";
  const offset = Math.max(0, Number.parseInt(params.get("offset") || "0", 10) || 0);
  const [data, setData] = useState(null);
  const [assignees, setAssignees] = useState([]);
  const [events, setEvents] = useState({ events: [], total: 0 });
  const [error, setError] = useState(null);
  const [pending, setPending] = useState(false);
  const [editing, setEditing] = useState(false);
  const [revision, setRevision] = useState(0);
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const root = `/api/workspaces/${encodeURIComponent(workspaceId)}/issues`;
  const base = `/w/${encodeURIComponent(workspaceId)}/work/${mine ? "mine" : "tasks"}`;
  const query = params.toString() ? `?${params.toString()}` : "";
  const inboxPath = `/w/${encodeURIComponent(workspaceId)}/work/inbox`;
  const returnTo = params.get("returnTo");
  const inboxReturn = returnTo && !returnTo.includes("#") && (returnTo === inboxPath || returnTo.startsWith(`${inboxPath}?`)) ? returnTo : null;
  const listUrl = inboxReturn || `${base}${query}`;
  const creating = issueId === "new";
  const detail = Boolean(issueId) && !creating;
  const title = t(mine ? "work.myIssues" : "work.issues");
  // biome-ignore lint/correctness/useExhaustiveDependencies: Revision explicitly reloads authoritative records after a mutation or user retry.
  useEffect(() => {
    const controller = new AbortController();
    const options = { signal: controller.signal };
    setData(null); setError(null); setEditing(false); setEvents({ events: [], total: 0 });
    const load = async () => {
      try {
        const [result, people, activity, catalog] = await Promise.all([
          creating ? Promise.resolve({}) : apiJson(detail ? `${root}/${encodeURIComponent(issueId)}` : `${root}?mine=${mine}&archived=${archived}&limit=${pageSize}&offset=${offset}`, options),
          apiJson(`${root}/assignees`, options),
          detail ? apiJson(`${root}/${encodeURIComponent(issueId)}/events?limit=${pageSize}&offset=0`, options) : Promise.resolve({ events: [], total: 0 }),
          creating ? apiJson(`/api/workspaces/${encodeURIComponent(workspaceId)}/issue-templates`, options) : Promise.resolve({ templates: [] }),
        ]);
        if (controller.signal.aborted) return;
        setTemplates(catalog.templates); setData(result); setAssignees(people.assignees); setEvents(activity);
      } catch (failure) { if (!controller.signal.aborted) setError(failure); }
    };
    void load();
    return () => controller.abort();
  }, [root, issueId, creating, detail, mine, archived, offset, revision]);

  const mutate = async body => {
    setPending(true); setError(null);
    try {
      const result = await apiJson(creating ? root : `${root}/${encodeURIComponent(issueId)}`, jsonOptions(creating ? "POST" : "PATCH", creating ? body : { ...body, version: data.issue.version }));
      if (!mounted.current) return;
      if (creating) navigate(`${base}/${encodeURIComponent(result.issue.id)}${query}`);
      else { setData(result); setEditing(false); setRevision(value => value + 1); }
    } catch (failure) { if (mounted.current) setError(failure); }
    finally { if (mounted.current) setPending(false); }
  };
  const personName = value => assignees.find(person => person.id === value)?.name || value || t("issues.unassigned");
  const fieldValue = (field, value) => field === "assigneeId" ? personName(value) : field === "status" ? value === null ? "" : t(`issues.status.${value}`) : (field === "archivedAt" || field === "archived") ? t(value ? "issues.archived" : "issues.active") : String(value ?? "");
  const time = value => new Date(value).toLocaleString(i18n.language);
  const issue = data?.issue;
  return <section className="issuesSurface" aria-busy={!data && !error}>
    <header className="workHeader"><WorkBreadcrumb parents={issueId ? [{ to: listUrl, label: inboxReturn ? t("work.inbox") : title }] : []} current={creating ? t("issues.new") : detail ? issue?.title || "" : title} count={!issueId ? data?.total : undefined} />
      {!issueId ? <Button variant="outline" onClick={() => navigate(`${base}/new${query}`)}><Plus aria-hidden="true" />{t("issues.new")}</Button> : null}
    </header>
    {error ? <div className="issueError" role="alert"><span>{error.status === 409 ? t("issues.conflict") : t("issues.failed", { message: error.message })}</span><Button variant="ghost" disabled={pending} onClick={() => setRevision(value => value + 1)}>{t("issues.reload")}</Button></div> : null}
    {!issueId ? <>
      <div className="issueFilters workTabs"><Button variant="ghost" aria-pressed={!archived} onClick={() => setParams({})}>{t("issues.active")}</Button><Button variant="ghost" aria-pressed={archived} onClick={() => setParams({ archived: "true" })}>{t("issues.archived")}</Button></div>
      {data ? <div className="issueList">{data.issues.map(item => <Link className="issueRow" key={item.id} to={`${base}/${encodeURIComponent(item.id)}${query}`}><IssueStatus status={item.status} /><span>{item.title}</span><small>{personName(item.assigneeId)}</small></Link>)}
        {data.total > pageSize ? <nav className="issuePagination" aria-label={t("issues.pages")}><Button variant="ghost" disabled={!offset} onClick={() => setParams({ ...(archived ? { archived: "true" } : {}), offset: String(Math.max(0, offset - pageSize)) })}>{t("issues.previous")}</Button><Button variant="ghost" disabled={offset + pageSize >= data.total} onClick={() => setParams({ ...(archived ? { archived: "true" } : {}), offset: String(offset + pageSize) })}>{t("issues.next")}</Button></nav> : null}
      </div> : null}
    </> : data ? <div className={`issueDetail ${detail && !editing ? "isReading" : ""}`}>
      {creating || editing ? <IssueForm key={issue?.version || "new"} issue={issue} assignees={assignees} defaultAssigneeId={mine ? userId : null} templates={templates} initialTemplateKey={params.get("template") || ""} pending={pending} onSave={mutate} onCancel={() => creating ? navigate(listUrl) : setEditing(false)} /> : issue ? <>
        <div className="issueHeading"><h1>{issue.title}</h1><div className="issueActions">
          <Button variant="ghost" size="icon" aria-label={t("issues.edit")} onClick={() => setEditing(true)}><Pencil aria-hidden="true" /></Button>
          <Menu.Root modal={false}><Menu.Trigger className="issueMoreActions" aria-label={t("issues.moreActions")}><Ellipsis aria-hidden="true" /></Menu.Trigger>
            <Menu.Portal><Menu.Positioner className="workMenuPositioner" align="end" sideOffset={5}><Menu.Popup className="workMenu">
              <Menu.Item className="workMenuItem" disabled={pending} onClick={() => void mutate({ archived: !issue.archivedAt })}><Archive aria-hidden="true" /><span>{t(issue.archivedAt ? "issues.restore" : "issues.archive")}</span></Menu.Item>
            </Menu.Popup></Menu.Positioner></Menu.Portal>
          </Menu.Root>
        </div></div>
        <aside className="issueProperties" aria-label={t("issues.properties")}><h2>{t("issues.properties")}</h2>
          <ChoiceMenu label={t("issues.status")} value={issue.status} options={issueStatuses.map(value => ({ value, label: t(`issues.status.${value}`), icon: <IssueStatus status={value} /> }))} disabled={pending} onChange={status => void mutate({ status })} />
          <ChoiceMenu label={t("issues.assignee")} value={issue.assigneeId || ""} options={[{ value: "", label: t("issues.unassigned") }, ...assignees.map(person => ({ value: person.id, label: person.name }))]} icon={<UserRound aria-hidden="true" />} disabled={pending} onChange={assigneeId => void mutate({ assigneeId: assigneeId || null })} />
          {issue.archivedAt ? <span className="issueArchived">{t("issues.archived")}</span> : null}
          {issue.templateKey ? <IssueTemplateValues definition={issue.templateSnapshot.definition} values={issue.properties} placement="properties" /> : null}
        </aside>
        <div className="issueDetailBody">
          {issue.description ? <p className="issueDescription">{issue.description}</p> : null}
          {issue.templateKey ? <IssueTemplateValues definition={issue.templateSnapshot.definition} values={issue.properties} placement="content" /> : null}
          <IssueMaterials workspaceId={workspaceId} issue={issue} onChanged={() => setRevision(value => value + 1)} />
          <IssueExecutions key={issue.id} workspaceId={workspaceId} userId={userId} issue={issue} onChanged={() => setRevision(value => value + 1)} />
          <IssueDeliverables workspaceId={workspaceId} userId={userId} issue={issue} onChanged={() => setRevision(value => value + 1)} />
        </div>
      </> : null}
      {issue ? <details className="issueActivity"><summary><ChevronRight aria-hidden="true" />{t("issues.activity")}</summary><ol>{events.events.map(event => <li key={event.id}><div><span>{event.actor.name}</span><span>{t(`issues.event.${event.type}`)}</span><time dateTime={event.createdAt}>{time(event.createdAt)}</time></div>{Object.entries(event.changes).filter(([field]) => field !== "deliverableId").map(([field, change]) => <p key={field}>{t(`issues.field.${field}`)}: <span>{fieldValue(field, change.before)}</span> → <span>{fieldValue(field, change.after)}</span></p>)}</li>)}</ol>{events.events.length < events.total ? <Button variant="ghost" disabled={pending} onClick={async () => { setPending(true); setError(null); try { const next = await apiJson(`${root}/${encodeURIComponent(issueId)}/events?limit=${pageSize}&offset=${events.events.length}`); if (mounted.current) setEvents(previous => ({ ...next, events: [...previous.events, ...next.events] })); } catch (failure) { if (mounted.current) setError(failure); } finally { if (mounted.current) setPending(false); } }}>{t("issues.moreActivity")}</Button> : null}</details> : null}
    </div> : null}
  </section>;
}
