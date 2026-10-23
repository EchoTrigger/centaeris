import { CalendarClock, ChevronRight, Clock, MessageSquarePlus, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invokeHost } from "../host/hostBridge";
import { Button } from "./ui/button";
import { ManagementBack, ManagementEmpty, ManagementError, ManagementFilter, ManagementHeader, ManagementLoading, ManagementSearch, ManagementStatus } from "./management/ManagementPage";

type Plan = { id: string; enabled: boolean; nextAt: number | null; spec: { name: string; prompt: string; cwd: string; timezone: string; cron: string | null; at: number | null } };
type Run = { id: string; scheduledAt: number; status: string; sessionId: string | null; error: string | null };
const errorMessage = (error: unknown) => error instanceof Error ? error.message : String(error);
function date(value: number | null, timezone?: string) {
  if (value === null) return "No upcoming run";
  const time = new Date(value);
  try { return time.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short", ...(timezone ? { timeZone: timezone } : {}) }); }
  catch { return time.toLocaleString(); }
}
const statusLabel = (status: string) => status.replace(/_/g, " ").replace(/^./, character => character.toUpperCase());
export function SchedulesPage({ onSession, onCreate }: { onSession(id: string): void; onCreate?: () => void }) {
  const [focusOnNavigation, setFocusOnNavigation] = useState(false);
  const [plans, setPlans] = useState<Plan[]>([]), [serviceEnabled, setServiceEnabled] = useState<boolean | null>(null);
  const [selected, setSelected] = useState<Plan | null>(null), [runs, setRuns] = useState<Run[]>([]);
  const [query, setQuery] = useState(""), [filter, setFilter] = useState("all");
  const [loading, setLoading] = useState(true), [runsLoading, setRunsLoading] = useState(false);
  const [nextPlans, setNextPlans] = useState<string | null>(null), [nextRuns, setNextRuns] = useState<string | null>(null);
  const [planFailure, setPlanFailure] = useState<{ message: string; cursor: string | null } | null>(null);
  const [runFailure, setRunFailure] = useState<{ message: string; cursor: string | null } | null>(null);
  const listRequest = useRef(0), runRequest = useRef(0), selectedId = useRef<string | null>(null);
  const loadPlans = useCallback(async (cursor: string | null = null) => {
    const request = ++listRequest.current;
    setLoading(true); setPlanFailure(null);
    try {
      const result = await invokeHost<{ schedules: Plan[]; serviceEnabled: boolean; nextCursor: string | null }>("schedule_manage", { request: { action: "list", ...(cursor ? { cursor } : {}) } });
      if (request !== listRequest.current) return;
      setPlans(previous => cursor ? [...previous, ...result.schedules.filter(plan => !previous.some(item => item.id === plan.id))] : result.schedules);
      setServiceEnabled(result.serviceEnabled); setNextPlans(result.nextCursor ?? null);
      setSelected(previous => previous ? result.schedules.find(plan => plan.id === previous.id) ?? previous : null);
    } catch (error) { if (request === listRequest.current) setPlanFailure({ message: errorMessage(error), cursor }); }
    finally { if (request === listRequest.current) setLoading(false); }
  }, []);
  const loadRuns = useCallback(async (scheduleId: string, cursor: string | null = null) => {
    const request = ++runRequest.current;
    setRunsLoading(true); setRunFailure(null);
    try {
      const result = await invokeHost<{ runs: Run[]; nextCursor: string | null }>("schedule_manage", { request: { action: "history", scheduleId, ...(cursor ? { cursor } : {}) } });
      if (request !== runRequest.current || selectedId.current !== scheduleId) return;
      setRuns(previous => cursor ? [...previous, ...result.runs.filter(run => !previous.some(item => item.id === run.id))] : result.runs);
      setNextRuns(result.nextCursor ?? null);
    } catch (error) { if (request === runRequest.current && selectedId.current === scheduleId) setRunFailure({ message: errorMessage(error), cursor }); }
    finally { if (request === runRequest.current && selectedId.current === scheduleId) setRunsLoading(false); }
  }, []);
  useEffect(() => { void loadPlans(); return () => { listRequest.current++; runRequest.current++; }; }, [loadPlans]);
  const open = (plan: Plan) => { setFocusOnNavigation(true); selectedId.current = plan.id; setSelected(plan); setRuns([]); setNextRuns(null); setRunFailure(null); void loadRuns(plan.id); };
  const back = () => { setFocusOnNavigation(true); selectedId.current = null; runRequest.current++; setSelected(null); setRunsLoading(false); setRunFailure(null); };
  const visible = useMemo(() => plans.filter(plan => (filter === "all" || filter === "enabled" && plan.enabled || filter === "paused" && !plan.enabled) && `${plan.spec.name} ${plan.spec.prompt} ${plan.spec.cwd}`.toLowerCase().includes(query.trim().toLowerCase())), [plans, filter, query]);
  const create = onCreate ? <Button variant="outline" onClick={onCreate}><MessageSquarePlus />Create with agent</Button> : null;
  return <section className="desktopManagement" aria-label="Scheduled"><main className="dmPage">
    {selected ? <>
      <ManagementBack onClick={back}>Back to schedules</ManagementBack>
      <ManagementHeader focusOnNavigation={focusOnNavigation} title={selected.spec.name} actions={<Button variant="ghost" aria-label="Refresh run history" disabled={runsLoading} onClick={() => void loadRuns(selected.id)}><RefreshCw />Refresh history</Button>} />
      <div className="dmMeta"><ManagementStatus tone={selected.enabled ? "success" : "neutral"}>{selected.enabled ? "Enabled" : "Paused"}</ManagementStatus><span>{selected.spec.cron ? "Recurring" : "One-time"}</span></div>
      <section className="dmSection"><h2>Instructions</h2><p className="dmPrompt">{selected.spec.prompt}</p></section>
      <section className="dmSection"><h2>Schedule</h2><dl className="dmFacts"><div><dt>Next run</dt><dd>{date(selected.nextAt, selected.spec.timezone)}</dd></div><div><dt>Timezone</dt><dd>{selected.spec.timezone}</dd></div><div><dt>Repeat</dt><dd>{selected.spec.cron ?? (selected.spec.at !== null && selected.spec.at !== undefined ? date(selected.spec.at, selected.spec.timezone) : "One-time")}</dd></div><div><dt>Workspace</dt><dd>{selected.spec.cwd}</dd></div></dl></section>
      <section className="dmSection" aria-label="Run history"><h2>Run history <ManagementStatus>{runs.length}</ManagementStatus></h2>
        {runFailure ? <ManagementError onRetry={() => void loadRuns(selected.id, runFailure.cursor)}>Unable to load run history. {runFailure.message}</ManagementError> : null}
        {runsLoading ? <ManagementLoading>{runs.length ? "Refreshing run history…" : "Loading run history…"}</ManagementLoading> : null}
        {!runs.length && !runsLoading && !runFailure ? <ManagementEmpty title="No runs yet" /> : null}
        {runs.slice().sort((a, b) => b.scheduledAt - a.scheduledAt || b.id.localeCompare(a.id)).map(run => <article className="dmRun" key={run.id} aria-label={`Run ${date(run.scheduledAt)}`}><div><time>{date(run.scheduledAt, selected.spec.timezone)}</time><ManagementStatus tone={run.status === "completed" || run.status === "succeeded" ? "success" : run.error || run.status === "failed" ? "error" : "neutral"}>{statusLabel(run.status)}</ManagementStatus></div>{run.sessionId ? <Button variant="outline" size="sm" onClick={() => { if (run.sessionId) onSession(run.sessionId); }}>Open chat</Button> : null}{run.error ? <ManagementError>{run.error}</ManagementError> : null}</article>)}
        {nextRuns ? <div className="dmPagination"><Button variant="outline" disabled={runsLoading} onClick={() => void loadRuns(selected.id, nextRuns)}>Load more runs</Button></div> : null}
      </section>
    </> : <>
      <ManagementHeader focusOnNavigation={focusOnNavigation} title="Scheduled tasks" actions={<><Button variant="ghost" aria-label="Refresh schedules" disabled={loading} onClick={() => void loadPlans()}><RefreshCw />Refresh</Button>{create}</>} />
      {serviceEnabled !== null ? <div className="dmService"><Clock size={15} aria-hidden="true" /><ManagementStatus tone={serviceEnabled ? "success" : "neutral"}>{serviceEnabled ? "Scheduling enabled" : "Scheduling paused"}</ManagementStatus><span>{serviceEnabled ? "Runs while this computer is awake." : "Existing plans are kept."}</span></div> : null}
      <div className="dmToolbar"><ManagementSearch label="Search schedules" value={query} onChange={setQuery} /><ManagementFilter label="Schedule status" value={filter} onChange={setFilter} options={[["all", "All schedules"], ["enabled", "Enabled"], ["paused", "Paused"]]} /><span className="dmCount">{visible.length} of {plans.length} schedules</span></div>
      {planFailure ? <ManagementError onRetry={() => void loadPlans(planFailure.cursor)}>Unable to load schedules. {planFailure.message}</ManagementError> : null}
      {loading ? <ManagementLoading>{plans.length ? "Refreshing schedules…" : "Loading schedules…"}</ManagementLoading> : null}
      {visible.length ? <div className="dmList" aria-label="Schedules" aria-busy={loading}>{visible.map(plan => <Button variant="ghost" className="dmRow" key={plan.id} aria-label={`Open schedule ${plan.spec.name}`} onClick={() => open(plan)}><span className="dmRowIcon"><CalendarClock /></span><span className="dmRowCopy"><strong>{plan.spec.name}</strong><small>{plan.nextAt !== null ? `Next: ${date(plan.nextAt, plan.spec.timezone)}` : "No upcoming run"}</small></span><span className="dmRowEnd"><ManagementStatus tone={plan.enabled ? "success" : "neutral"}>{plan.enabled ? "Enabled" : "Paused"}</ManagementStatus><ChevronRight size={15} aria-hidden="true" /></span></Button>)}</div> : !loading && !planFailure ? <ManagementEmpty title={plans.length ? "No matching schedules" : "No schedules yet"} action={plans.length ? <Button variant="outline" onClick={() => { setQuery(""); setFilter("all"); }}>Clear filters</Button> : create} /> : null}
      {nextPlans ? <div className="dmPagination"><Button variant="outline" disabled={loading} onClick={() => void loadPlans(nextPlans)}>Load more schedules</Button></div> : null}
    </>}
  </main></section>;
}
