import { useEffect, useRef, useState } from "react";
import { FolderOpen, MapPin, Plus } from "lucide-react";
import { selectSkillSourcePath, type SkillSourceKind } from "../../lib/chatBridge";
import type { ConfirmAction } from "../ConfirmDialog";
import { Button } from "../ui/button";
import { Input } from "../ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../ui/select";
import { ManagementError } from "./ManagementPage";

export type AddSkillLocationRequest = { scope: "workspace" | "user"; kind: SkillSourceKind; path: string; workspaceRoot: string | null };
export function AddSkillLocation({ workspaceRoot, pending, confirmAction, onAdd, onCancel }: {
  workspaceRoot?: string | null; pending: boolean; confirmAction: ConfirmAction;
  onAdd(request: AddSkillLocationRequest): Promise<boolean>; onCancel(): void;
}) {
  const initialScope = workspaceRoot ? "workspace" : "user";
  const [scope, setScope] = useState<"workspace" | "user">(initialScope);
  const [kind, setKind] = useState<SkillSourceKind>("catalogDirectory");
  const [path, setPath] = useState("");
  const [browsing, setBrowsing] = useState(false);
  const [error, setError] = useState<{ message: string; retryBrowse: boolean } | null>(null);
  const [confirming, setConfirming] = useState(false);
  const pickerOwner = useRef(0);
  useEffect(() => () => { pickerOwner.current++; }, []);
  const dirty = Boolean(path.trim()) || scope !== initialScope || kind !== "catalogDirectory";
  const cancel = async () => {
    if (pending || confirming) return;
    if (!dirty) { onCancel(); return; }
    const owner = pickerOwner.current; setConfirming(true);
    try {
      const confirmed = await confirmAction({ title: "Discard this location?", message: "The location has not been added. Discard your changes and return to locations?" });
      if (owner === pickerOwner.current && confirmed) onCancel();
    } catch (failure) { if (owner === pickerOwner.current) setError({ message: failure instanceof Error ? failure.message : String(failure), retryBrowse: false }); }
    finally { if (owner === pickerOwner.current) setConfirming(false); }
  };
  const browse = async () => {
    const owner = ++pickerOwner.current;
    setBrowsing(true); setError(null);
    try {
      const selected = await selectSkillSourcePath(kind);
      if (owner === pickerOwner.current && !selected.cancelled && selected.path) setPath(selected.path);
    } catch (failure) { if (owner === pickerOwner.current) setError({ message: failure instanceof Error ? failure.message : String(failure), retryBrowse: true }); }
    finally { if (owner === pickerOwner.current) setBrowsing(false); }
  };
  return <form className="dmForm" aria-label="Add skill location" onSubmit={event => {
    event.preventDefault();
    if (pending || browsing || !path.trim() || scope === "workspace" && !workspaceRoot) return;
    void onAdd({ scope, kind, path: path.trim(), workspaceRoot: scope === "workspace" ? workspaceRoot ?? null : null });
  }}>
    <div className="dmField"><label htmlFor="skill-location-scope">Scope</label><Select value={scope} onValueChange={next => { if (next === "workspace" || next === "user") setScope(next); }} disabled={pending || browsing}>
      <SelectTrigger id="skill-location-scope" aria-label="Location scope"><SelectValue>{scope === "workspace" ? "Current workspace" : "Personal"}</SelectValue></SelectTrigger><SelectContent alignItemWithTrigger={false}><SelectItem value="workspace" disabled={!workspaceRoot}>Current workspace</SelectItem><SelectItem value="user">Personal</SelectItem></SelectContent>
    </Select><p>{scope === "workspace" ? "Available only when working in this workspace." : "Available across your workspaces on this computer."}</p></div>
    <div className="dmField"><label htmlFor="skill-location-kind">Location type</label><Select value={kind} onValueChange={next => { if (next === "skillFile" || next === "catalogDirectory") { pickerOwner.current++; setKind(next); setPath(""); setError(null); } }} disabled={pending || browsing}>
      <SelectTrigger id="skill-location-kind" aria-label="Location type"><SelectValue>{kind === "catalogDirectory" ? "Catalog directory" : "SKILL.md file"}</SelectValue></SelectTrigger><SelectContent alignItemWithTrigger={false}><SelectItem value="catalogDirectory">Catalog directory</SelectItem><SelectItem value="skillFile">SKILL.md file</SelectItem></SelectContent>
    </Select><p>{kind === "catalogDirectory" ? "Add a directory containing your skill folders." : "Add one skill by selecting its SKILL.md file."}</p></div>
    <div className="dmField"><label htmlFor="skill-location-path">Path</label><div className="dmPathField"><Input id="skill-location-path" autoFocus value={path} disabled={pending || browsing} onChange={event => setPath(event.target.value)} placeholder={kind === "skillFile" ? "Path to SKILL.md" : "Path to skill directory"} /><Button variant="outline" disabled={pending || browsing} onClick={() => void browse()}><FolderOpen />{browsing ? "Choosing…" : "Browse"}</Button></div></div>
    {scope === "workspace" && workspaceRoot ? <div className="dmBinding"><MapPin size={15} aria-hidden="true" /><span>{workspaceRoot}</span></div> : null}
    {error ? <ManagementError onRetry={error.retryBrowse ? () => void browse() : undefined} retryLabel="Retry browse">{error.message}</ManagementError> : null}
    <div className="dmFormFooter"><Button variant="outline" disabled={pending || confirming} onClick={() => void cancel()}>Cancel</Button><Button type="submit" disabled={pending || browsing || !path.trim() || scope === "workspace" && !workspaceRoot}><Plus />{pending ? "Adding…" : "Add location"}</Button></div>
  </form>;
}
