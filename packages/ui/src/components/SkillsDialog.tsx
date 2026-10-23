import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { BookOpen, ChevronRight, FolderOpen, Plus, RefreshCw, Settings2, Trash2 } from "lucide-react";
import { addSkillSource, getSkillCatalog, getSkillDetail, listSkillSources, reloadSkillCatalog, removeSkillSource, revealSkillSource, setSkillEnabled, setSkillSourceEnabled, type SkillCatalogSnapshot, type SkillDetail, type SkillEntry, type SkillSourceConfig, type SkillSourcesConfig } from "../lib/chatBridge";
import { MarkdownContent } from "./chat/MarkdownContent";
import type { ConfirmAction } from "./ConfirmDialog";
import { Button } from "./ui/button";
import { AddSkillLocation, type AddSkillLocationRequest } from "./management/AddSkillLocation";
import { ManagementBack, ManagementEmpty, ManagementError, ManagementFilter, ManagementHeader, ManagementLoading, ManagementSearch, ManagementStatus, ManagementToggle } from "./management/ManagementPage";

type Selection = { kind: "skill"; id: string; fromSource?: string } | { kind: "source"; id: string } | { kind: "sources" } | { kind: "add" } | null;
const emptySources: SkillSourcesConfig = { schemaVersion: "skill.sources.v1", sources: [], skillPolicies: [] };
const normalize = (path?: string | null) => String(path || "").trim().replace(/^\\\\\?\\/, "").replace(/\\/g, "/").replace(/\/+$/, "");
const leaf = (path: string) => path.split(/[\\/]/).filter(Boolean).at(-1) || path;
const scopeName = (scope: string) => ({ workspace: "Workspace", user: "Personal", system: "System", plugin: "Plugin" })[scope] ?? scope;
const message = (error: unknown) => error instanceof Error ? error.message : String(error || "Skill request failed");
function skillStatus(skill: SkillEntry) { return skill.errors.length ? "Needs attention" : skill.shadowedBy ? "Shadowed" : skill.enabled ? "Enabled" : "Disabled"; }

export function SkillsDialog(props: { workspaceRoot?: string | null; confirmAction: ConfirmAction }) {
  return <WorkspaceSkills key={normalize(props.workspaceRoot)} {...props} />;
}
function WorkspaceSkills({ workspaceRoot, confirmAction }: { workspaceRoot?: string | null; confirmAction: ConfirmAction }) {
  const [focusOnNavigation, setFocusOnNavigation] = useState(false);
  const [query, setQuery] = useState(""), [sourceQuery, setSourceQuery] = useState("");
  const [scope, setScope] = useState("all"), [status, setStatus] = useState("all");
  const [sources, setSources] = useState(emptySources);
  const [catalog, setCatalog] = useState<SkillCatalogSnapshot | null>(null);
  const [selection, setSelection] = useState<Selection>(null);
  const selectionRef = useRef(selection); selectionRef.current = selection;
  const navigationRevision = useRef(0);
  const [detail, setDetail] = useState<SkillDetail | null>(null);
  const [loading, setLoading] = useState(true), [detailLoading, setDetailLoading] = useState(false);
  const [loadError, setLoadError] = useState(""), [actionError, setActionError] = useState(""), [detailError, setDetailError] = useState("");
  const [notice, setNotice] = useState(""), [pending, setPending] = useState("");
  const owner = useRef(0), loadRequest = useRef(0), detailRequest = useRef(0), mutationBusy = useRef(false);
  const load = useCallback(async (reload = false) => {
    const request = ++loadRequest.current, epoch = owner.current;
    setLoading(true); setLoadError("");
    try {
      const [nextSources, nextCatalog] = await Promise.all([listSkillSources(), reload ? reloadSkillCatalog(workspaceRoot) : getSkillCatalog(workspaceRoot)]);
      if (epoch !== owner.current || request !== loadRequest.current) return;
      setSources(nextSources); setCatalog(nextCatalog);
    } catch (error) { if (epoch === owner.current && request === loadRequest.current) setLoadError(message(error)); }
    finally { if (epoch === owner.current && request === loadRequest.current) setLoading(false); }
  }, [workspaceRoot]);
  useEffect(() => { void load(); return () => { owner.current++; loadRequest.current++; detailRequest.current++; }; }, [load]);
  const navigate = (next: Selection) => { navigationRevision.current++; setFocusOnNavigation(true); detailRequest.current++; setSelection(next); setDetail(null); setDetailError(""); setDetailLoading(false); setActionError(""); };
  const openSkill = async (skill: SkillEntry, fromSource?: string) => {
    navigationRevision.current++;
    setFocusOnNavigation(true);
    const request = ++detailRequest.current, epoch = owner.current;
    setSelection({ kind: "skill", id: skill.skillId, fromSource }); setDetail(null); setDetailError(""); setActionError(""); setDetailLoading(true);
    try {
      const next = await getSkillDetail({ cwd: workspaceRoot, skillId: skill.skillId });
      if (epoch === owner.current && request === detailRequest.current) setDetail(next);
    } catch (error) { if (epoch === owner.current && request === detailRequest.current) setDetailError(message(error)); }
    finally { if (epoch === owner.current && request === detailRequest.current) setDetailLoading(false); }
  };
  const mutate = async (key: string, action: (current: () => boolean) => Promise<void>) => {
    if (mutationBusy.current) return false;
    mutationBusy.current = true; setPending(key); setActionError(""); setNotice("");
    const epoch = owner.current, navigation = navigationRevision.current;
    const current = () => epoch === owner.current;
    try { await action(current); return current(); }
    catch (error) { if (current() && navigation === navigationRevision.current) setActionError(message(error)); return false; }
    finally { mutationBusy.current = false; if (current()) setPending(""); }
  };
  const visibleSources = useMemo(() => sources.sources.filter(source => source.scope === "user" || source.scope === "workspace" && normalize(source.workspaceRoot) === normalize(workspaceRoot)), [sources.sources, workspaceRoot]);
  const matchingSources = visibleSources.filter(source => `${source.path} ${source.scope}`.toLowerCase().includes(sourceQuery.trim().toLowerCase()));
  const skills = (catalog?.skills ?? []).filter(skill => skill.scope !== "plugin");
  const visibleSkills = skills.filter(skill => (scope === "all" || (scope === "personal" ? skill.scope === "user" || skill.scope === "workspace" : skill.scope === "system")) &&
    (status === "all" || status === "enabled" && skill.enabled && !skill.errors.length && !skill.shadowedBy || status === "disabled" && !skill.enabled || status === "attention" && Boolean(skill.errors.length || skill.shadowedBy)) &&
    `${skill.name} ${skill.description} ${skill.skillMdPath}`.toLowerCase().includes(query.trim().toLowerCase())).sort((a, b) => a.name.localeCompare(b.name));
  const skillGroups = [
    { name: "Personal", items: visibleSkills.filter(skill => skill.scope === "user" || skill.scope === "workspace") },
    { name: "System", items: visibleSkills.filter(skill => skill.scope === "system") },
  ].filter(group => group.items.length > 0);
  const selectedSkill = selection?.kind === "skill" ? skills.find(skill => skill.skillId === selection.id) : undefined;
  const selectedSource = selection?.kind === "source" ? visibleSources.find(source => source.sourceId === selection.id) : undefined;
  const knownDetail = selectedSkill && detail?.skill.skillId === selectedSkill.skillId ? detail : null;
  const add = (request: AddSkillLocationRequest) => mutate("add", async current => {
    const receipt = await addSkillSource(request);
    if (!current()) return;
    setSources(receipt); navigate({ kind: "sources" }); setNotice("Location added.");
    await load(true);
  });
  const toggleSkill = (skill: SkillEntry) => mutate(`skill:${skill.skillId}`, async current => {
    const receipt = await setSkillEnabled({ cwd: workspaceRoot, skillId: skill.skillId, enabled: !skill.enabled });
    if (!current()) return;
    // A mutation receipt supersedes reads started before that mutation completed.
    loadRequest.current++; setLoading(false); setLoadError(""); setCatalog(receipt);
    const updated = receipt.skills.find(candidate => candidate.skillId === skill.skillId);
    setDetail(previous => previous?.skill.skillId === skill.skillId && updated ? { ...previous, skill: updated } : previous);
    setNotice(`${skill.name} ${updated?.enabled ? "enabled" : "disabled"}.`);
  });
  const toggleSource = (source: SkillSourceConfig) => mutate(`source:${source.sourceId}`, async current => {
    const receipt = await setSkillSourceEnabled({ sourceId: source.sourceId, enabled: !source.enabled });
    if (!current()) return;
    setSources(receipt); setNotice(`${leaf(source.path)} ${source.enabled ? "disabled" : "enabled"}.`); await load(true);
  });
  const revealSource = (source: SkillSourceConfig) => mutate("reveal", async () => {
    const receipt = await revealSkillSource(source.sourceId);
    if (!receipt.opened) throw new Error("The location could not be opened. Check that it is available on this computer.");
  });
  const removeSource = async (source: SkillSourceConfig) => {
    const epoch = owner.current;
    try {
      const confirmed = await confirmAction({ title: "Remove this skill location?", message: `${leaf(source.path)} will be removed from Centaeris. Files on disk stay unchanged.` });
      if (!confirmed || epoch !== owner.current) return;
      await mutate(`source:${source.sourceId}`, async current => {
        const receipt = await removeSkillSource(source.sourceId);
        if (!current()) return;
        setSources(receipt);
        if (selectionRef.current?.kind === "source" && selectionRef.current.id === source.sourceId) navigate({ kind: "sources" });
        setNotice("Location removed. Files on disk stay unchanged."); await load(true);
      });
    } catch (error) { if (epoch === owner.current) setActionError(message(error)); }
  };
  const refresh = <Button variant="ghost" size="icon-sm" aria-label="Reload skills" disabled={loading || Boolean(pending)} onClick={() => void load(true)}><RefreshCw /></Button>;
  return <section className="desktopManagement" aria-label="Skills"><main className="dmPage">
    {selection?.kind === "add" ? <>
      <ManagementHeader focusOnNavigation={focusOnNavigation} title="Add skill location" />
      {actionError ? <ManagementError>{actionError}</ManagementError> : null}
      <AddSkillLocation workspaceRoot={workspaceRoot} pending={pending === "add"} confirmAction={confirmAction} onAdd={add} onCancel={() => navigate({ kind: "sources" })} />
    </> : <>
      {selection ? <ManagementBack onClick={() => navigate(selection.kind === "skill" && selection.fromSource ? { kind: "source", id: selection.fromSource } : selection.kind === "source" ? { kind: "sources" } : null)}>{selection.kind === "skill" && selection.fromSource ? "Back to location" : selection.kind === "source" ? "Back to locations" : "Back to skills"}</ManagementBack> : null}
      {loadError ? <ManagementError onRetry={() => void load(true)}>Unable to refresh skills. {loadError}</ManagementError> : null}
      {actionError ? <ManagementError>{actionError}</ManagementError> : null}
      {notice ? <p className="dmNotice" role="status">{notice}</p> : null}
      {selection?.kind === "sources" ? <>
        <ManagementHeader focusOnNavigation={focusOnNavigation} title="Skill locations" actions={<>{refresh}<Button variant="outline" disabled={Boolean(pending)} onClick={() => navigate({ kind: "add" })}><Plus />Add location</Button></>} />
        <div className="dmToolbar"><ManagementSearch label="Search skill locations" value={sourceQuery} onChange={setSourceQuery} /><span className="dmCount">{matchingSources.length} of {visibleSources.length} locations</span></div>
        {loading ? <ManagementLoading>Refreshing locations…</ManagementLoading> : null}
        <div className="dmList">{matchingSources.map(source => <Button variant="ghost" className="dmRow" key={source.sourceId} aria-label={`Open location ${leaf(source.path)}`} onClick={() => navigate({ kind: "source", id: source.sourceId })}><span className="dmRowIcon"><FolderOpen /></span><span className="dmRowCopy"><strong>{leaf(source.path)}</strong><small>{scopeName(source.scope)} — {source.path}</small></span><span className="dmRowEnd"><ManagementStatus>{source.enabled ? "Enabled" : "Disabled"}</ManagementStatus><ChevronRight size={15} /></span></Button>)}</div>
        {!loading && visibleSources.length > 0 && !matchingSources.length ? <ManagementEmpty title="No matching locations" action={<Button variant="outline" onClick={() => setSourceQuery("")}>Clear search</Button>} /> : null}
        {!loading && !visibleSources.length ? <ManagementEmpty title="No skill locations" action={<Button variant="outline" onClick={() => navigate({ kind: "add" })}><Plus />Add location</Button>} /> : null}
      </> : selectedSource ? <>
        <ManagementHeader focusOnNavigation={focusOnNavigation} title={leaf(selectedSource.path)} description={`${scopeName(selectedSource.scope)} skill location`} actions={<ManagementToggle checked={selectedSource.enabled} disabled={Boolean(pending)} label={selectedSource.enabled ? "Disable location" : "Enable location"} onChange={() => void toggleSource(selectedSource)} />} />
        <div className="dmActions"><Button variant="outline" disabled={Boolean(pending)} onClick={() => void revealSource(selectedSource)}><FolderOpen />Show in folder</Button>{selectedSource.scope === "user" || selectedSource.scope === "workspace" ? <Button variant="destructive" disabled={Boolean(pending)} onClick={() => void removeSource(selectedSource)}><Trash2 />Remove location</Button> : null}</div>
        <section className="dmSection"><h2>Location</h2><dl className="dmFacts"><div><dt>Path</dt><dd>{selectedSource.path}</dd></div><div><dt>Type</dt><dd>{selectedSource.kind === "skillFile" ? "SKILL.md file" : "Catalog directory"}</dd></div><div><dt>Scope</dt><dd>{scopeName(selectedSource.scope)}</dd></div>{selectedSource.workspaceRoot ? <div><dt>Workspace</dt><dd>{selectedSource.workspaceRoot}</dd></div> : null}</dl></section>
        {(catalog?.diagnostics ?? []).filter(item => item.sourceId === selectedSource.sourceId).map(item => <ManagementError key={`${item.code}:${item.message}`}>{item.message}</ManagementError>)}
        <section className="dmSection"><h2>Skills in this location</h2>{skills.filter(skill => skill.sourceId === selectedSource.sourceId).length ? <div className="dmList">{skills.filter(skill => skill.sourceId === selectedSource.sourceId).map(skill => <SkillRow key={skill.skillId} skill={skill} onClick={() => void openSkill(skill, selectedSource.sourceId)} />)}</div> : <p>{selectedSource.enabled ? "No skills found." : "Location disabled."}</p>}</section>
      </> : selectedSkill ? <>
        <ManagementHeader focusOnNavigation={focusOnNavigation} title={selectedSkill.name} description={selectedSkill.description} actions={<ManagementToggle checked={selectedSkill.enabled} disabled={Boolean(pending) || selectedSkill.errors.length > 0} label={selectedSkill.enabled ? "Disable skill" : "Enable skill"} onChange={() => void toggleSkill(selectedSkill)} />} />
        <div className="dmMeta"><ManagementStatus tone={selectedSkill.errors.length || selectedSkill.shadowedBy ? "warning" : selectedSkill.enabled ? "success" : "neutral"}>{skillStatus(selectedSkill)}</ManagementStatus><span>{scopeName(selectedSkill.scope)}</span><span>{selectedSkill.allowImplicitInvocation ? "Available to the agent" : "Explicit invocation only"}</span></div>
        {selectedSkill.errors.map(item => <ManagementError key={item}>{item}</ManagementError>)}
        {selectedSkill.shadowedBy ? <ManagementError>A skill with higher priority overrides this skill: {selectedSkill.shadowedBy}</ManagementError> : null}
        <section className="dmSection"><h2>Location and access</h2><dl className="dmFacts"><div><dt>SKILL.md</dt><dd>{selectedSkill.skillMdPath}</dd></div><div><dt>Allowed tools</dt><dd>{selectedSkill.capabilityMetadata.allowedTools.length ? selectedSkill.capabilityMetadata.allowedTools.join(", ") : "Uses the session’s tool permissions"}</dd></div></dl>{visibleSources.some(source => source.sourceId === selectedSkill.sourceId) ? <Button className="mt-3" variant="outline" onClick={() => navigate({ kind: "source", id: selectedSkill.sourceId })}><Settings2 />Manage location</Button> : null}</section>
        <section className="dmSection"><h2>Instructions</h2>{detailLoading ? <ManagementLoading>Loading SKILL.md…</ManagementLoading> : detailError ? <ManagementError onRetry={() => void openSkill(selectedSkill, selection?.kind === "skill" ? selection.fromSource : undefined)}>Unable to load instructions. {detailError}</ManagementError> : knownDetail ? <div className="dmMarkdown"><MarkdownContent text={knownDetail.content} /></div> : <p>Instructions unavailable. Refresh the catalog and reopen this skill.</p>}</section>
      </> : selection ? <ManagementEmpty title="This item is unavailable" action={refresh} /> : <>
        <ManagementHeader focusOnNavigation={focusOnNavigation} title="Skills" actions={<>{refresh}<Button variant="outline" aria-label="Manage skill locations" onClick={() => navigate({ kind: "sources" })}><Settings2 />Locations</Button><Button variant="outline" onClick={() => navigate({ kind: "add" })}><Plus />Add location</Button></>} />
        <div className="dmToolbar"><ManagementSearch label="Search skills" value={query} onChange={setQuery} /><ManagementFilter label="Skill scope" value={scope} onChange={setScope} options={[["all", "All sources"], ["personal", "Personal"], ["system", "System"]]} /><ManagementFilter label="Skill status" value={status} onChange={setStatus} options={[["all", "All statuses"], ["enabled", "Enabled"], ["disabled", "Disabled"], ["attention", "Needs attention"]]} /></div>
        {loading ? <ManagementLoading>{catalog ? "Refreshing skills…" : "Loading skills…"}</ManagementLoading> : null}
        {(catalog?.diagnostics ?? []).map(item => <ManagementError key={`${item.sourceId ?? "catalog"}:${item.code}:${item.message}`}>{item.message}{item.sourceId && visibleSources.some(source => source.sourceId === item.sourceId) ? <div className="dmDiagnosticAction"><Button variant="outline" size="sm" onClick={() => { if (item.sourceId) navigate({ kind: "source", id: item.sourceId }); }}>Review location</Button></div> : null}</ManagementError>)}
        {visibleSkills.length ? <div className="dmList" aria-label="Available skills" aria-busy={loading}>{skillGroups.map(group => <section className="dmSkillGroup" role="group" aria-label={`${group.name} skills`} key={group.name}><h2>{group.name}</h2>{group.items.map(skill => <SkillRow key={skill.skillId} skill={skill} onClick={() => void openSkill(skill)} />)}</section>)}</div> : !loading && !loadError ? <ManagementEmpty title={skills.length ? "No matching skills" : "No skills available"} action={skills.length ? <Button variant="outline" onClick={() => { setQuery(""); setScope("all"); setStatus("all"); }}>Clear filters</Button> : <Button variant="outline" onClick={() => navigate({ kind: "sources" })}>Manage locations</Button>} /> : null}
      </>}
    </>}
  </main></section>;
}
function SkillRow({ skill, onClick }: { skill: SkillEntry; onClick(): void }) {
  return <Button variant="ghost" className="dmRow" aria-label={`Open ${skill.name}`} onClick={onClick}><span className="dmRowIcon"><BookOpen /></span><span className="dmRowCopy"><strong>{skill.name}</strong><small>{skill.description || scopeName(skill.scope)}</small></span><span className="dmRowEnd"><ManagementStatus tone={skill.errors.length || skill.shadowedBy ? "warning" : skill.enabled ? "success" : "neutral"}>{skillStatus(skill)}</ManagementStatus><ChevronRight size={15} aria-hidden="true" /></span></Button>;
}
export default SkillsDialog;
