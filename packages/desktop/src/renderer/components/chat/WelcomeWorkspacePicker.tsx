import { useEffect, useId, useRef, useState } from "react";
import { ChevronUp, Folder, FolderOpen } from "lucide-react";
import { t } from "../../i18n";
import type { WorkspaceInfo } from "../../lib/workspaceBridge";
import { Button } from "../ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuRadioGroup, DropdownMenuRadioItem, DropdownMenuTrigger } from "../ui/dropdown-menu";

export function WelcomeWorkspacePicker({ workspace, workspaces, onOpenWorkspace, onSelectWorkspace, onFocusComposer, inactive = false }: {
  workspace?: Pick<WorkspaceInfo, "root" | "name">;
  workspaces: readonly WorkspaceInfo[];
  onOpenWorkspace: () => Promise<void>;
  onSelectWorkspace: (root: string) => Promise<void>;
  onFocusComposer?: () => void;
  inactive?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const errorId = useId();
  const busy = useRef(false);
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  useEffect(() => { if (inactive) setOpen(false); }, [inactive]);
  const run = async (action: () => Promise<void>) => {
    if (busy.current) return;
    busy.current = true; setPending(true); setError("");
    try { await action(); if (mounted.current) setOpen(false); onFocusComposer?.(); }
    catch (failure) { if (mounted.current) setError(failure instanceof Error ? failure.message : String(failure)); }
    finally { busy.current = false; if (mounted.current) setPending(false); }
  };
  return <div className="desktopWelcomePicker">
    <DropdownMenu open={open && !inactive} onOpenChange={setOpen}>
      <DropdownMenuTrigger render={<Button variant="ghost" className="desktopWelcomeChoose" disabled={pending} aria-label={workspace ? t("welcome.chooseWorkspace") + ": " + workspace.name : t("welcome.chooseWorkspace")} aria-describedby={error ? errorId : undefined} />}>
        <Folder aria-hidden="true" /><span>{workspace?.name ?? t("welcome.chooseWorkspace")}</span><ChevronUp aria-hidden="true" />
      </DropdownMenuTrigger>
      <DropdownMenuContent side="top" align="start" sideOffset={8} className="desktopWelcomeWorkspaceMenu">
        <DropdownMenuRadioGroup value={workspace?.root ?? ""} onValueChange={root => void run(() => onSelectWorkspace(root))}>
          {workspaces.map(item => <DropdownMenuRadioItem key={item.root} value={item.root} disabled={pending}>
            <Folder aria-hidden="true" /><span className="desktopWelcomePickerCopy"><span>{item.name}</span><small>{item.root}</small></span>
          </DropdownMenuRadioItem>)}
        </DropdownMenuRadioGroup>
        <DropdownMenuItem disabled={pending} onClick={() => void run(onOpenWorkspace)}><FolderOpen aria-hidden="true" /><span>{t("welcome.openFolder")}</span><span className="desktopWelcomePickerShortcut">Ctrl+O</span></DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
    {error ? <p id={errorId} className="workbenchInlineError" role="alert">{error}</p> : null}
  </div>;
}
