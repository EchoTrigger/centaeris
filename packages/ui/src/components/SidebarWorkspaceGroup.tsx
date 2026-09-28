import { useRef, useState, type ReactNode } from "react";
import { Folder } from "lucide-react";
import { ChatActionsMenu } from "./ChatActionsMenu";

export function SidebarWorkspaceGroup({ root, name, children, onRename, onRemove, onExpand, initiallyExpanded = true }: {
  onExpand?: (root: string) => void;
  initiallyExpanded?: boolean;
  root: string;
  name: string;
  children: ReactNode;
  onRename?: (root: string, name: string) => Promise<boolean>;
  onRemove?: (root: string) => Promise<boolean>;
}) {
  const [expanded, setExpanded] = useState(initiallyExpanded);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(name);
  const [pending, setPending] = useState(false);
  const pendingRef = useRef(false);
  const run = async (action: () => Promise<boolean>) => {
    if (pendingRef.current) return;
    pendingRef.current = true;
    setPending(true);
    try { if (await action()) setEditing(false); }
    finally { pendingRef.current = false; setPending(false); }
  };
  return <section className="sidebarWorkspaceGroup" aria-label={`Workspace ${root}`}>
    <div className="sidebarWorkspaceHeading">
      {editing ? <form onSubmit={event => {
        event.preventDefault();
        if (draft.trim() && onRename) void run(() => onRename(root, draft.trim()));
      }}>
        <input aria-label="Workspace name" autoFocus value={draft} disabled={pending}
          onChange={event => setDraft(event.target.value)}
          onKeyDown={event => { if (event.key === "Escape") setEditing(false); }} />
        <button type="submit" disabled={pending || !draft.trim()}>Save</button>
      </form> : <button className="sidebarWorkspaceToggle" type="button" title={root}
        aria-label={`Toggle ${name}`} aria-expanded={expanded} onClick={() => { if (!expanded) onExpand?.(root); setExpanded(value => !value); }}>
        <Folder aria-hidden="true" /><span>{name}</span>
      </button>}
      <ChatActionsMenu label={`Workspace actions ${name}`}>
        <button type="button" disabled={pending || !onRename} onClick={() => { setDraft(name); setEditing(true); }}>Rename</button>
        <button type="button" disabled={pending || !onRemove} onClick={() => { if (onRemove) void run(() => onRemove(root)); }}>Remove</button>
      </ChatActionsMenu>
    </div>
    {expanded ? children : null}
  </section>;
}
