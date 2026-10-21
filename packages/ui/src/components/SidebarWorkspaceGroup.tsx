import { useRef, useState, type ReactNode } from "react";
import { Collapsible } from "@base-ui/react/collapsible";
import {
	Check,
	ChevronRight,
	Folder,
	Loader2,
	MoreHorizontal,
	Pencil,
	Trash2,
} from "lucide-react";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from "./ui/dialog";
import {
	DropdownMenu,
	DropdownMenuContent,
	DropdownMenuItem,
	DropdownMenuTrigger,
} from "./ui/dropdown-menu";
import type { WorkspaceAction } from "./WorkspaceSwitcher";
import "../styles/desktop-workbench.css";

export function SidebarWorkspaceGroup({
	root,
	name,
	children,
	onRename,
	onRemove,
	onExpand,
	onSelect,
	active = false,
	empty = false,
	initiallyExpanded = true,
}: {
	onExpand?: (root: string) => void;
	onSelect?: (root: string) => WorkspaceAction;
	active?: boolean;
	empty?: boolean;
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
	const [error, setError] = useState("");
	const pendingRef = useRef(false);
	const menuButton = useRef<HTMLButtonElement>(null);
	const input = useRef<HTMLInputElement>(null);
	const run = async (action: () => WorkspaceAction, closeEditor = false) => {
		if (pendingRef.current) return;
		pendingRef.current = true;
		setPending(true);
		setError("");
		try {
			if ((await action()) === false)
				throw new Error("The workspace action could not be completed.");
			if (closeEditor) setEditing(false);
		} catch (failure) {
			setError(failure instanceof Error ? failure.message : String(failure));
		} finally {
			pendingRef.current = false;
			setPending(false);
		}
	};
	return (
		<section
			className="sidebarWorkspaceGroup desktopWorkspaceGroupNav"
			aria-label={`Workspace ${root}`}
			data-current={active || undefined}
		>
			<Collapsible.Root
				open={expanded}
				onOpenChange={(next) => {
					if (next) onExpand?.(root);
					setExpanded(next);
				}}
			>
				<div className="sidebarWorkspaceHeading">
					<Collapsible.Trigger
						render={
							<Button
								variant="ghost"
								size="icon-sm"
								className="desktopWorkspaceDisclosure"
								aria-label={`Toggle ${name}`}
							/>
						}
					>
						<ChevronRight size={14} aria-hidden="true" />
					</Collapsible.Trigger>
					<Button
						variant="ghost"
						className="desktopWorkspaceSelect"
						aria-label={`Select workspace ${name}`}
						aria-current={active ? "location" : undefined}
						title={root}
						aria-busy={pending}
						disabled={pending || !onSelect}
						onClick={() => {
							if (onSelect && !active) void run(() => onSelect(root));
						}}
					>
						{pending ? (
							<Loader2
								size={14}
								className="workbenchSpinner"
								aria-hidden="true"
							/>
						) : (
							<Folder size={14} aria-hidden="true" />
						)}
						<span>{name}</span>
						{active ? <Check size={12} aria-label="Current workspace" /> : null}
					</Button>
					<DropdownMenu>
						<DropdownMenuTrigger
							render={
								<Button
									ref={menuButton}
									variant="ghost"
									size="icon-sm"
									aria-label={`Workspace actions ${name}`}
									className="desktopRowMenu"
								/>
							}
						>
							<MoreHorizontal size={16} aria-hidden="true" />
						</DropdownMenuTrigger>
						<DropdownMenuContent align="end">
							<DropdownMenuItem
								disabled={pending || !onRename}
								onClick={() => {
									setDraft(name);
									setError("");
									setEditing(true);
								}}
							>
								<Pencil size={15} aria-hidden="true" />
								Rename
							</DropdownMenuItem>
							<DropdownMenuItem
								variant="destructive"
								disabled={pending || !onRemove}
								onClick={() => {
									if (onRemove) void run(() => onRemove(root));
								}}
							>
								<Trash2 size={15} aria-hidden="true" />
								Remove workspace
							</DropdownMenuItem>
						</DropdownMenuContent>
					</DropdownMenu>
				</div>
				<Collapsible.Panel className="desktopWorkspaceConversations">
					{children}
					{empty ? (
						<p className="desktopWorkspaceEmpty">No conversations yet</p>
					) : null}
				</Collapsible.Panel>
			</Collapsible.Root>
			{error && !editing ? (
				<p role="alert" className="workbenchInlineError">
					{error}
				</p>
			) : null}
			<Dialog
				open={editing}
				onOpenChange={(next, details) => {
					if (pending) details.cancel();
					else setEditing(next);
				}}
			>
				<DialogContent
					className="workbenchDialog"
					initialFocus={input}
					finalFocus={menuButton}
					showCloseButton={!pending}
				>
					<DialogHeader>
						<DialogTitle>Rename workspace</DialogTitle>
						<DialogDescription>{root}</DialogDescription>
					</DialogHeader>
					<form
						onSubmit={(event) => {
							event.preventDefault();
							if (draft.trim() && onRename)
								void run(() => onRename(root, draft.trim()), true);
						}}
					>
						<label htmlFor={`workspace-name-${root}`}>Workspace name</label>
						<Input
							ref={input}
							id={`workspace-name-${root}`}
							value={draft}
							disabled={pending}
							onChange={(event) => setDraft(event.target.value)}
							onFocus={(event) => event.currentTarget.select()}
						/>
						{error ? (
							<p role="alert" className="workbenchInlineError">
								{error}
							</p>
						) : null}
						<DialogFooter>
							<Button
								variant="outline"
								type="button"
								disabled={pending}
								onClick={() => setEditing(false)}
							>
								Cancel
							</Button>
							<Button type="submit" disabled={pending || !draft.trim()}>
								{pending ? "Saving…" : "Save"}
							</Button>
						</DialogFooter>
					</form>
				</DialogContent>
			</Dialog>
		</section>
	);
}
