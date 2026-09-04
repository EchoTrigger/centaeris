import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  BookOpen,
  Check,
  FilePlus2,
  FileText,
  FolderOpen,
  MapPin,
  PackagePlus,
  Plus,
  RefreshCw,
  Trash2,
} from "lucide-react";
import {
  addSkillSource,
  getSkillCatalog,
  getSkillDetail,
  listSkillSources,
  reloadSkillCatalog,
  removeSkillSource,
  revealSkillSource,
  selectSkillSourcePath,
  setSkillEnabled,
  setSkillSourceEnabled,
  type SkillCatalogSnapshot,
  type SkillDetail,
  type SkillEntry,
  type SkillSourceConfig,
  type SkillSourceKind,
  type SkillSourcesConfig,
} from "../lib/chatBridge";
import { MarkdownContent } from "./chat/MarkdownContent";
import type { ConfirmAction } from "./ConfirmDialog";

type SkillSelection =
  | { kind: "skill"; id: string }
  | { kind: "source"; id: string }
  | { kind: "add" }
  | null;

type SkillsDialogProps = {
  workspaceRoot?: string | null;
  confirmAction: ConfirmAction;
};

const emptySources: SkillSourcesConfig = {
  schemaVersion: "skill.sources.v1",
  sources: [],
  skillPolicies: [],
};

const errorMessage = (error: unknown): string =>
  error instanceof Error ? error.message : String(error || "skill request failed");

const normalizePath = (value?: string | null): string =>
  String(value || "")
    .trim()
    .replace(/^\\\\\?\\/, "")
    .replace(/\\/g, "/")
    .replace(/\/+$/, "");

const pathLeaf = (value: string): string =>
  value.split(/[\\/]/).filter(Boolean).at(-1) || value;

const scopeLabel = (source: SkillSourceConfig): string => {
  switch (source.scope) {
    case "workspace": return "Workspace";
    case "user": return "User";
    case "system": return "System";
    case "plugin": return "Plugin";
  }
};

const skillIcon = (name: string) => {
  if (name === "skill-creator") return <FilePlus2 aria-hidden="true" />;
  if (name === "skill-installer") return <PackagePlus aria-hidden="true" />;
  return <BookOpen aria-hidden="true" />;
};

export function SkillsDialog({ workspaceRoot, confirmAction }: SkillsDialogProps) {
  const [sources, setSources] = useState<SkillSourcesConfig>(emptySources);
  const [catalog, setCatalog] = useState<SkillCatalogSnapshot | null>(null);
  const [selection, setSelection] = useState<SkillSelection>(null);
  const [detail, setDetail] = useState<SkillDetail | null>(null);
  const [loading, setLoading] = useState(true);
  const [detailLoadingId, setDetailLoadingId] = useState("");
  const [pendingKey, setPendingKey] = useState("");
  const [error, setError] = useState("");
  const loadSequence = useRef(0);
  const detailSequence = useRef(0);

  const load = useCallback(async (reload = false) => {
    const requestId = ++loadSequence.current;
    setLoading(true);
    setError("");
    try {
      const nextSources = await listSkillSources();
      if (requestId !== loadSequence.current) return;
      setSources(nextSources);
      const nextCatalog = reload
        ? await reloadSkillCatalog(workspaceRoot)
        : await getSkillCatalog(workspaceRoot);
      if (requestId !== loadSequence.current) return;
      setCatalog(nextCatalog);
    } catch (loadError) {
      if (requestId !== loadSequence.current) return;
      setCatalog(null);
      setError(errorMessage(loadError));
    } finally {
      if (requestId === loadSequence.current) setLoading(false);
    }
  }, [workspaceRoot]);

  useEffect(() => {
    detailSequence.current += 1;
    setSelection(null);
    setDetail(null);
    setDetailLoadingId("");
    void load();
  }, [load]);

  const visibleSources = useMemo(() => sources.sources.filter((source) => {
    if (source.scope !== "workspace") return true;
    return normalizePath(source.workspaceRoot) === normalizePath(workspaceRoot);
  }), [sources.sources, workspaceRoot]);

  const skillsBySource = useMemo(() => {
    const groups = new Map<string, SkillEntry[]>();
    for (const skill of catalog?.skills ?? []) {
      const group = groups.get(skill.sourceId) ?? [];
      group.push(skill);
      groups.set(skill.sourceId, group);
    }
    for (const group of groups.values()) group.sort((left, right) => left.name.localeCompare(right.name));
    return groups;
  }, [catalog]);

  const selectedSource = selection?.kind === "source"
    ? visibleSources.find((source) => source.sourceId === selection.id) ?? null
    : null;
  const selectedSkill = selection?.kind === "skill"
    ? catalog?.skills.find((skill) => skill.skillId === selection.id) ?? null
    : null;

  const openSkill = async (skill: SkillEntry) => {
    const requestId = ++detailSequence.current;
    setSelection({ kind: "skill", id: skill.skillId });
    setDetail(null);
    setDetailLoadingId(skill.skillId);
    setError("");
    try {
      const nextDetail = await getSkillDetail({ cwd: workspaceRoot, skillId: skill.skillId });
      if (requestId === detailSequence.current) setDetail(nextDetail);
    } catch (detailError) {
      if (requestId === detailSequence.current) setError(errorMessage(detailError));
    } finally {
      if (requestId === detailSequence.current) setDetailLoadingId("");
    }
  };

  const selectWithoutDetail = (nextSelection: SkillSelection) => {
    detailSequence.current += 1;
    setSelection(nextSelection);
    setDetail(null);
    setDetailLoadingId("");
  };

  const mutate = async (key: string, action: () => Promise<void>) => {
    setPendingKey(key);
    setError("");
    try {
      await action();
    } catch (mutationError) {
      setError(errorMessage(mutationError));
    } finally {
      setPendingKey("");
    }
  };

  return (
    <div className="skillsDialogLayout">
      <aside className="skillsSidebar">
        <div className="skillsSourceList">
          {visibleSources.length === 0 && !loading ? (
            <p className="skillsEmptySidebar">No skill locations</p>
          ) : null}
          {visibleSources.map((source) => (
            <section className="skillsSourceGroup" key={source.sourceId}>
              <button
                type="button"
                className={selection?.kind === "source" && selection.id === source.sourceId ? "skillsSourceButton is-active" : "skillsSourceButton"}
                onClick={() => selectWithoutDetail({ kind: "source", id: source.sourceId })}
              >
                <span className="skillsSourceIcon">{source.scope === "system" ? <BookOpen /> : source.kind === "skillFile" ? <FileText /> : <FolderOpen />}</span>
                <span className="skillsSourceCopy">
                  <strong>{pathLeaf(source.path)}</strong>
                  <small>{scopeLabel(source)}</small>
                </span>
                <i className={source.enabled ? "is-enabled" : ""} aria-label={source.enabled ? "Enabled" : "Disabled"} />
              </button>
              <div className="skillsSourceChildren">
                {(skillsBySource.get(source.sourceId) ?? []).map((skill) => (
                  <button
                    type="button"
                    key={skill.skillId}
                    className={selection?.kind === "skill" && selection.id === skill.skillId ? "is-active" : ""}
                    onClick={() => void openSkill(skill)}
                  >
                    <span className="skillsItemLabel">{skillIcon(skill.name)}<span>{skill.name}</span></span>
                    {skill.enabled && !skill.shadowedBy && skill.errors.length === 0 ? <Check /> : null}
                  </button>
                ))}
                {source.enabled && (skillsBySource.get(source.sourceId) ?? []).length === 0 ? (
                  <span className="skillsSourceEmpty">No valid skills</span>
                ) : null}
              </div>
            </section>
          ))}
        </div>
        <button type="button" className="modelsAddButton modelsAddProvider skillsAddLocation" onClick={() => selectWithoutDetail({ kind: "add" })}>
          <Plus aria-hidden="true" /> Add location
        </button>
      </aside>

      <main className="skillsMain">
        <div className="skillsToolbar">
          <span>{catalog ? `${catalog.skills.length} skills` : "Skills"}</span>
          <button type="button" onClick={() => void load(true)} disabled={loading} title="Reload">
            <RefreshCw aria-hidden="true" />
          </button>
        </div>
        <div className="skillsMainScroll">
          {selection?.kind === "add" ? (
            <AddSkillLocation
              workspaceRoot={workspaceRoot}
              pending={pendingKey === "add"}
              onAdd={(request) => mutate("add", async () => {
                await addSkillSource(request);
                await load(true);
                setSelection(null);
              })}
            />
          ) : selectedSource ? (
            <SkillSourceDetail
              source={selectedSource}
              diagnostics={(catalog?.diagnostics ?? []).filter((item) => item.sourceId === selectedSource.sourceId)}
              pending={pendingKey === `source:${selectedSource.sourceId}`}
              onToggle={() => mutate(`source:${selectedSource.sourceId}`, async () => {
                await setSkillSourceEnabled({ sourceId: selectedSource.sourceId, enabled: !selectedSource.enabled });
                await load(true);
              })}
              onReveal={() => mutate(`source:${selectedSource.sourceId}`, async () => {
                await revealSkillSource(selectedSource.sourceId);
              })}
              onRemove={() => mutate(`source:${selectedSource.sourceId}`, async () => {
                const confirmed = await confirmAction({
                  title: "Remove this Skill location?",
                  message: `“${pathLeaf(selectedSource.path)}” will be removed from Centaeris. Files on disk stay unchanged.`,
                });
                if (!confirmed) return;
                await removeSkillSource(selectedSource.sourceId);
                setSelection(null);
                await load(true);
              })}
            />
          ) : selectedSkill ? (
            <SkillDetailView
              skill={detail?.skill ?? selectedSkill}
              content={detail?.content ?? ""}
              loading={detailLoadingId === selectedSkill.skillId}
              pending={pendingKey === `skill:${selectedSkill.skillId}`}
              onToggle={() => mutate(`skill:${selectedSkill.skillId}`, async () => {
                const nextCatalog = await setSkillEnabled({
                  cwd: workspaceRoot,
                  skillId: selectedSkill.skillId,
                  enabled: !selectedSkill.enabled,
                });
                setCatalog(nextCatalog);
                const nextSkill = nextCatalog.skills.find((item) => item.skillId === selectedSkill.skillId);
                if (nextSkill && detail) setDetail({ ...detail, skill: nextSkill });
              })}
            />
          ) : (
            <div className="skillsEmptyMain">
              <span>Select a skill</span>
            </div>
          )}
        </div>
        {error ? <div className="skillsError" role="status">{error}</div> : null}
      </main>
    </div>
  );
}

function AddSkillLocation({
  workspaceRoot,
  pending,
  onAdd,
}: {
  workspaceRoot?: string | null;
  pending: boolean;
  onAdd: (request: { scope: "workspace" | "user"; kind: SkillSourceKind; path: string; workspaceRoot?: string | null }) => void;
}) {
  const [scope, setScope] = useState<"workspace" | "user">(workspaceRoot ? "workspace" : "user");
  const [kind, setKind] = useState<SkillSourceKind>("catalogDirectory");
  const [path, setPath] = useState("");

  const browse = async () => {
    const selection = await selectSkillSourcePath(kind);
    if (!selection.cancelled && selection.path) setPath(selection.path);
  };

  return (
    <section className="skillsForm">
      <h2>Add skill location</h2>
      <p>Register one exact location. Centaeris does not search conventional skill folders.</p>
      <label>
        <span>Scope</span>
        <div className="skillsSegmented">
          <button type="button" className={scope === "workspace" ? "is-active" : ""} disabled={!workspaceRoot} onClick={() => setScope("workspace")}>Workspace</button>
          <button type="button" className={scope === "user" ? "is-active" : ""} onClick={() => setScope("user")}>User</button>
        </div>
      </label>
      <label>
        <span>Location type</span>
        <div className="skillsSegmented">
          <button type="button" className={kind === "catalogDirectory" ? "is-active" : ""} onClick={() => { setKind("catalogDirectory"); setPath(""); }}>Catalog directory</button>
          <button type="button" className={kind === "skillFile" ? "is-active" : ""} onClick={() => { setKind("skillFile"); setPath(""); }}>SKILL.md</button>
        </div>
      </label>
      <label>
        <span>Path</span>
        <div className="skillsPathField">
          <input value={path} onChange={(event) => setPath(event.target.value)} placeholder={kind === "skillFile" ? "C:\\path\\to\\SKILL.md" : "C:\\path\\to\\catalog"} />
          <button type="button" onClick={() => void browse()}><FolderOpen aria-hidden="true" /> Browse</button>
        </div>
      </label>
      {scope === "workspace" && workspaceRoot ? <div className="skillsWorkspaceBinding"><MapPin />{workspaceRoot}</div> : null}
      <button
        type="button"
        className="skillsPrimaryButton"
        disabled={pending || !path.trim() || (scope === "workspace" && !workspaceRoot)}
        onClick={() => onAdd({ scope, kind, path: path.trim(), workspaceRoot: scope === "workspace" ? workspaceRoot : null })}
      >Add location</button>
    </section>
  );
}

function SkillSourceDetail({
  source,
  diagnostics,
  pending,
  onToggle,
  onReveal,
  onRemove,
}: {
  source: SkillSourceConfig;
  diagnostics: Array<{ code: string; message: string }>;
  pending: boolean;
  onToggle: () => void;
  onReveal: () => void;
  onRemove: () => void;
}) {
  const userManaged = source.scope === "workspace" || source.scope === "user";
  return (
    <section className="skillsDetail">
      <header className="skillsDetailHeader">
        <div><span className="skillsEyebrow">{scopeLabel(source)} location</span><h2>{pathLeaf(source.path)}</h2></div>
        <button type="button" className="resourceSwitch" onClick={onToggle} disabled={pending} aria-label={source.enabled ? "Disable location" : "Enable location"} aria-pressed={source.enabled}><span className={source.enabled ? "is-on" : ""} /></button>
      </header>
      <dl className="skillsFacts">
        <div><dt>Path</dt><dd>{source.path}</dd></div>
        <div><dt>Type</dt><dd>{source.kind === "catalogDirectory" ? "Catalog directory" : "SKILL.md"}</dd></div>
        {source.workspaceRoot ? <div><dt>Workspace</dt><dd>{source.workspaceRoot}</dd></div> : null}
        <div><dt>Source ID</dt><dd>{source.sourceId}</dd></div>
      </dl>
      {diagnostics.map((diagnostic) => <p className="skillsDiagnostic" key={`${diagnostic.code}:${diagnostic.message}`}>{diagnostic.message}</p>)}
      <div className="skillsDetailButtons">
        <button type="button" onClick={onReveal} disabled={pending}><FolderOpen /> Show in folder</button>
        {userManaged ? <button type="button" className="is-danger" onClick={onRemove} disabled={pending}><Trash2 /> Remove location</button> : null}
      </div>
    </section>
  );
}

function SkillDetailView({
  skill,
  content,
  loading,
  pending,
  onToggle,
}: {
  skill: SkillEntry;
  content: string;
  loading: boolean;
  pending: boolean;
  onToggle: () => void;
}) {
  return (
    <article className="skillsDetail">
      <header className="skillsDetailHeader">
        <div><span className="skillsEyebrow">{skill.scope} skill</span><h2>{skill.name}</h2><p>{skill.description}</p></div>
        <button type="button" className="resourceSwitch" onClick={onToggle} disabled={pending || skill.errors.length > 0} aria-label={skill.enabled ? "Disable skill" : "Enable skill"} aria-pressed={skill.enabled}><span className={skill.enabled ? "is-on" : ""} /></button>
      </header>
      <div className="skillsBadges">
        {!skill.allowImplicitInvocation ? <span>Explicit invocation only</span> : <span>Available to model</span>}
        {skill.shadowedBy ? <span>Shadowed</span> : null}
        {skill.capabilityMetadata.allowedTools.map((tool) => <code key={tool}>{tool}</code>)}
      </div>
      {skill.errors.map((item) => <p className="skillsDiagnostic" key={item}>{item}</p>)}
      <div className="skillsSkillPath">{skill.skillMdPath}</div>
      {loading ? <div className="skillsLoading">Loading SKILL.md...</div> : <div className="skillsMarkdown"><MarkdownContent text={content} /></div>}
    </article>
  );
}

export default SkillsDialog;
