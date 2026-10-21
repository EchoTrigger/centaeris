import { useId, useRef, useState } from "react";
import { ChevronDown, FolderOpen, Loader2, Plus } from "lucide-react";
import type { WorkspaceInfo, WorkspaceOpenMode } from "../lib/workspaceBridge";
import { Button } from "./ui/button";
import {
	DropdownMenu,
	DropdownMenuContent,
	DropdownMenuGroup,
	DropdownMenuLabel,
	DropdownMenuItem,
	DropdownMenuRadioGroup,
	DropdownMenuRadioItem,
	DropdownMenuSeparator,
	DropdownMenuTrigger,
} from "./ui/dropdown-menu";
import "../styles/desktop-workbench.css";

export type WorkspaceAction = void | boolean | Promise<void | boolean>;
export function WorkspaceSwitcher({
	workspaces,
	activeRoot,
	collapsed = false,
	onSelect,
	onOpen,
}: {
	workspaces: WorkspaceInfo[];
	activeRoot: string | null;
	collapsed?: boolean;
	onSelect: (root: string) => WorkspaceAction;
	onOpen: (mode: WorkspaceOpenMode) => WorkspaceAction;
}) {
	const [open, setOpen] = useState(false);
	const [busy, setBusy] = useState("");
	const [error, setError] = useState("");
	const pending = useRef(false);
	const pathId = useId();
	const current = workspaces.find((workspace) => workspace.root === activeRoot);
	const name =
		current?.name ||
		activeRoot?.split(/[\\/]/).filter(Boolean).at(-1) ||
		"Open a workspace";
	const recent = [...workspaces].sort(
		(a, b) => b.updatedAt - a.updatedAt || a.sortOrder - b.sortOrder,
	);
	const run = async (label: string, action: () => WorkspaceAction) => {
		if (pending.current) return;
		pending.current = true;
		setBusy(label);
		setError("");
		try {
			if ((await action()) === false)
				throw new Error("The workspace could not be opened. Try again.");
			setOpen(false);
		} catch (failure) {
			setError(failure instanceof Error ? failure.message : String(failure));
		} finally {
			pending.current = false;
			setBusy("");
		}
	};
	return (
		<div
			className="desktopWorkspaceSwitcher"
			data-collapsed={collapsed || undefined}
		>
			<DropdownMenu open={open} onOpenChange={setOpen}>
				<DropdownMenuTrigger
					render={
						<Button
							variant="ghost"
							className="desktopWorkspaceTrigger"
							disabled={Boolean(busy)}
							aria-busy={Boolean(busy)}
							aria-label={`Switch workspace: ${name}`}
							aria-describedby={pathId}
							title={activeRoot ?? "Open a folder or create a workspace"}
						/>
					}
				>
					{busy ? (
						<Loader2
							className="workbenchSpinner"
							size={17}
							aria-hidden="true"
						/>
					) : (
						<FolderOpen size={17} aria-hidden="true" />
					)}
					<span className="desktopWorkspaceIdentity" hidden={collapsed}>
						<strong>{name}</strong>
						<span id={collapsed ? undefined : pathId}>
							{busy || activeRoot || "Choose a folder to get started"}
						</span>
					</span>
					{collapsed ? (
						<span id={pathId} className="workbenchVisuallyHidden">
							{activeRoot || "No workspace selected"}
						</span>
					) : (
						<ChevronDown size={14} aria-hidden="true" />
					)}
				</DropdownMenuTrigger>
				<DropdownMenuContent
					align="start"
					side={collapsed ? "right" : "bottom"}
					className="desktopWorkspaceChoices"
					aria-label="Workspaces"
				>
					<DropdownMenuGroup>
						<DropdownMenuLabel>Recent workspaces</DropdownMenuLabel>
						{recent.length ? (
							<DropdownMenuRadioGroup
								value={activeRoot ?? ""}
								onValueChange={(value) => {
									if (value !== activeRoot)
										void run(
											`Opening ${recent.find((workspace) => workspace.root === value)?.name ?? "workspace"}…`,
											() => onSelect(String(value)),
										);
								}}
							>
								{recent.map((workspace) => (
									<DropdownMenuRadioItem
										key={workspace.root}
										value={workspace.root}
										disabled={Boolean(busy)}
										className="desktopWorkspaceChoice"
										title={workspace.root}
									>
										<FolderOpen size={16} aria-hidden="true" />
										<span>
											<strong>{workspace.name}</strong>
											<small>{workspace.root}</small>
										</span>
									</DropdownMenuRadioItem>
								))}
							</DropdownMenuRadioGroup>
						) : (
							<p className="desktopWorkspaceEmpty">No recent workspaces</p>
						)}
					</DropdownMenuGroup>
					<DropdownMenuSeparator />
					<DropdownMenuItem
						disabled={Boolean(busy)}
						onClick={() =>
							void run("Opening folder…", () => onOpen("customPath"))
						}
					>
						<FolderOpen size={16} aria-hidden="true" />
						Open folder…
					</DropdownMenuItem>
					<DropdownMenuItem
						disabled={Boolean(busy)}
						onClick={() =>
							void run("Creating workspace…", () => onOpen("defaultDirectory"))
						}
					>
						<Plus size={16} aria-hidden="true" />
						New workspace
					</DropdownMenuItem>
				</DropdownMenuContent>
			</DropdownMenu>
			{error ? (
				<p role="alert" className="workbenchInlineError">
					{error}
				</p>
			) : null}
		</div>
	);
}
