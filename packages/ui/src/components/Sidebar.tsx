import { recentWorkspaceGroups, sortSessionCatalog } from "./sidebarSessions";
import { SidebarWorkspaceGroup } from "./SidebarWorkspaceGroup";
import { WorkspaceSwitcher, type WorkspaceAction } from "./WorkspaceSwitcher";
import { SortablePinnedSessions } from "./SortablePinnedSessions";
import { useRef, useState, type ReactNode } from "react";
import {
	Blocks,
	Clock,
	Loader2,
	MoreHorizontal,
	Pencil,
	Pin,
	Settings,
	SquarePen,
	Trash2,
} from "lucide-react";
import type { UiSession } from "../types/ui";
import type {
	WorkspaceFileTreeEntry,
	WorkspaceInfo,
	WorkspaceOpenMode,
} from "../lib/workspaceBridge";
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
	DropdownMenuSeparator,
	DropdownMenuTrigger,
} from "./ui/dropdown-menu";
import { Tooltip, TooltipContent, TooltipTrigger } from "./ui/tooltip";
import "../styles/desktop-workbench.css";

export type ResourceModalKind = "models" | "skills" | "plugins";
type SidebarProps = {
	onExpandWorkspace?: (root: string) => void;
	onLoadMore?: (root: string) => void;
	hasMore?: (root: string) => boolean;
	onRenameWorkspace?: (root: string, name: string) => Promise<boolean>;
	onRemoveWorkspace?: (root: string) => Promise<boolean>;
	onReorderPinnedSessions?: (ids: string[]) => Promise<void>;
	onNavigate?: (page: "schedules" | "plugins" | "settings") => void;
	onPinSession?: (id: string, pinned: boolean) => Promise<void>;
	collapsed?: boolean;
	activePage?: string;
	sessions: UiSession[];
	currentSessionId: string | null;
	workspaces: WorkspaceInfo[];
	activeWorkspaceRoot: string | null;
	runningSessionIds: Set<string>;
	completedSessionIds: Set<string>;
	workspaceCatalogError: { message: string; canReset: boolean } | null;
	onNewChat: () => void;
	onOpenWorkspace: (mode: WorkspaceOpenMode) => WorkspaceAction;
	onSelectWorkspace: (root: string) => WorkspaceAction;
	onRetryWorkspaceCatalog: () => Promise<void>;
	onResetWorkspaceCatalog: () => Promise<void>;
	onSelectSession: (sessionId: string) => void;
	onRenameSession: (sessionId: string, title: string) => Promise<void>;
	onDeleteSession: (sessionId: string) => Promise<void>;
	onOpenResource: (kind: ResourceModalKind) => void;
	onOpenFile: (entry: WorkspaceFileTreeEntry) => void;
};

export function Sidebar({
	onExpandWorkspace,
	onLoadMore,
	hasMore,
	activeWorkspaceRoot,
	onRenameWorkspace,
	onRemoveWorkspace,
	onReorderPinnedSessions,
	workspaces,
	onNavigate,
	onPinSession,
	collapsed = false,
	activePage = "chat",
	sessions,
	currentSessionId,
	runningSessionIds,
	completedSessionIds,
	workspaceCatalogError,
	onNewChat,
	onOpenWorkspace,
	onSelectWorkspace,
	onRetryWorkspaceCatalog,
	onResetWorkspaceCatalog,
	onSelectSession,
	onRenameSession,
	onDeleteSession,
	onOpenResource,
}: SidebarProps) {
	const [dialog, setDialog] = useState<{
		kind: "rename" | "delete";
		session: UiSession;
	} | null>(null);
	const dialogContent = useRef(dialog);
	if (dialog) dialogContent.current = dialog;
	const currentDialog = dialogContent.current;
	const [draft, setDraft] = useState("");
	const [pending, setPending] = useState(false);
	const pendingRef = useRef(false);
	const [error, setError] = useState("");
	const [catalogBusy, setCatalogBusy] = useState(false);
	const menuButtons = useRef(new Map<string, HTMLButtonElement>());
	const input = useRef<HTMLInputElement>(null);
	const newChatButton = useRef<HTMLButtonElement>(null);
	const execute = async (action: () => Promise<void>, close = false) => {
		if (pendingRef.current) return;
		pendingRef.current = true;
		setPending(true);
		setError("");
		try {
			await action();
			if (close) setDialog(null);
		} catch (failure) {
			setError(failure instanceof Error ? failure.message : String(failure));
		} finally {
			pendingRef.current = false;
			setPending(false);
		}
	};
	const recoverCatalog = async (action: () => Promise<void>) => {
		if (catalogBusy) return;
		setCatalogBusy(true);
		setError("");
		try {
			await action();
		} catch (failure) {
			setError(failure instanceof Error ? failure.message : String(failure));
		} finally {
			setCatalogBusy(false);
		}
	};
	const pinnedSessions = sortSessionCatalog(
		sessions.filter((session) => session.isPinned),
	);
	const groups = recentWorkspaceGroups(sessions, workspaces);
	const renderSession = (session: UiSession, dragHandle?: ReactNode) => {
		const current = activePage === "chat" && session.id === currentSessionId;
		const running = runningSessionIds.has(session.id);
		const unread = completedSessionIds.has(session.id) || session.isUnread;
		return (
			<div
				className={`thinSessionRow ${current ? "is-active" : ""}`}
				key={session.id}
			>
				{dragHandle}
				<button
					type="button"
					className="thinSessionSelect"
					title={session.cwd}
					aria-current={current ? "page" : undefined}
					onClick={() => onSelectSession(session.id)}
				>
					<span
						className={`thinSessionDot ${running ? "is-running" : unread ? "is-unread" : ""}`}
						aria-hidden="true"
					/>
					<span className="thinSessionCopy">
						<strong>{session.title || "New chat"}</strong>
						<small>
							{running
								? "Running"
								: unread
									? "Unread"
									: formatRelativeTime(session.updatedAt)}
						</small>
					</span>
					{session.isPinned ? (
						<Pin className="thinPinnedIcon" aria-hidden="true" />
					) : null}
				</button>
				<div className="thinSessionActions">
					<DropdownMenu>
						<DropdownMenuTrigger
							render={
								<Button
									ref={(element) => {
										if (element) menuButtons.current.set(session.id, element);
										else menuButtons.current.delete(session.id);
									}}
									variant="ghost"
									size="icon-sm"
									aria-label={`Chat actions ${session.title || "New chat"}`}
									className="desktopRowMenu"
								/>
							}
						>
							<MoreHorizontal size={16} aria-hidden="true" />
						</DropdownMenuTrigger>
						<DropdownMenuContent align="end">
							{onPinSession ? (
								<>
									<DropdownMenuItem
										disabled={pending}
										aria-label={`${session.isPinned ? "Unpin" : "Pin"} ${session.title}`}
										onClick={() =>
											void execute(() =>
												onPinSession(session.id, !session.isPinned),
											)
										}
									>
										<Pin size={15} aria-hidden="true" />
										{session.isPinned ? "Unpin" : "Pin"}
									</DropdownMenuItem>
									<DropdownMenuSeparator />
								</>
							) : null}
							<DropdownMenuItem
								disabled={pending}
								aria-label={`Rename ${session.title}`}
								onClick={() => {
									setDraft(session.title);
									setError("");
									setDialog({ kind: "rename", session });
								}}
							>
								<Pencil size={15} aria-hidden="true" />
								Rename
							</DropdownMenuItem>
							<DropdownMenuItem
								disabled={pending}
								variant="destructive"
								aria-label={`Delete ${session.title}`}
								onClick={() => {
									setError("");
									setDialog({ kind: "delete", session });
								}}
							>
								<Trash2 size={15} aria-hidden="true" />
								Delete
							</DropdownMenuItem>
						</DropdownMenuContent>
					</DropdownMenu>
				</div>
			</div>
		);
	};
	const navigation = [
		{ label: "New chat", icon: SquarePen, action: onNewChat, current: false },
		{
			label: "Scheduled",
			icon: Clock,
			action: () => onNavigate?.("schedules"),
			current: activePage === "schedules",
		},
		{
			label: "Plugins",
			icon: Blocks,
			action: () =>
				onNavigate ? onNavigate("plugins") : onOpenResource("plugins"),
			current: activePage === "plugins",
		},
	];
	return (
		<aside
			className={`thinSidebar desktopWorkbenchSidebar ${collapsed ? "is-collapsed" : ""}`}
			aria-label="Main navigation"
		>
			<WorkspaceSwitcher
				workspaces={workspaces}
				activeRoot={activeWorkspaceRoot}
				collapsed={collapsed}
				onSelect={onSelectWorkspace}
				onOpen={onOpenWorkspace}
			/>
			<nav className="appNavigation" aria-label="Application">
				{navigation.map(({ label, icon: Icon, action, current }) => (
					<Tooltip key={label}>
						<TooltipTrigger
							render={
								<Button
									ref={label === "New chat" ? newChatButton : undefined}
									variant="ghost"
									aria-label={label}
									aria-current={current ? "page" : undefined}
									onClick={action}
								/>
							}
						>
							<Icon size={16} aria-hidden="true" />
							<span>{label}</span>
						</TooltipTrigger>
						<TooltipContent side="right">{label}</TooltipContent>
					</Tooltip>
				))}
			</nav>
			<div className="sidebarExpandedContent" hidden={collapsed}>
				{workspaceCatalogError ? (
					<div className="thinWorkspaceError" role="alert">
						<strong>
							{workspaceCatalogError.canReset
								? "Workspace list is corrupted"
								: "Workspace list unavailable"}
						</strong>
						<span>{workspaceCatalogError.message}</span>
						<div>
							<Button
								variant="outline"
								size="sm"
								disabled={catalogBusy}
								onClick={() => void recoverCatalog(onRetryWorkspaceCatalog)}
							>
								{catalogBusy ? (
									<Loader2 className="workbenchSpinner" aria-hidden="true" />
								) : null}
								Retry
							</Button>
							{workspaceCatalogError.canReset ? (
								<Button
									variant="ghost"
									size="sm"
									disabled={catalogBusy}
									onClick={() => void recoverCatalog(onResetWorkspaceCatalog)}
								>
									Reset list…
								</Button>
							) : null}
						</div>
					</div>
				) : null}
				{error && !dialog ? (
					<p role="alert" className="workbenchInlineError">
						{error}
					</p>
				) : null}
				<section className="thinSessionRegion" aria-label="Pinned">
					<div className="thinSectionHeader">
						<span>Pinned</span>
						{pinnedSessions.length ? (
							<small>{pinnedSessions.length}</small>
						) : null}
					</div>
					<div className="thinSessionList">
						{onReorderPinnedSessions && !hasMore?.("pinned") ? (
							<SortablePinnedSessions
								sessions={pinnedSessions}
								disabled={Boolean(dialog || pending || collapsed)}
								onReorder={onReorderPinnedSessions}
								renderSession={renderSession}
							/>
						) : (
							pinnedSessions.map((session) => renderSession(session))
						)}
						{!pinnedSessions.length ? (
							<p className="desktopWorkspaceEmpty">
								Pin conversations to keep them here
							</p>
						) : null}
						{hasMore?.("pinned") ? (
							<Button
								variant="ghost"
								size="sm"
								className="desktopLoadMore"
								onClick={() => onLoadMore?.("pinned")}
							>
								Load more
							</Button>
						) : null}
					</div>
				</section>
				<section className="thinSessionRegion" aria-label="Recents">
					<div className="thinSectionHeader">
						<span>Conversations</span>
					</div>
					<div className="thinSessionList">
						{groups.map((group) => (
							<SidebarWorkspaceGroup
								key={group.root}
								root={group.root}
								name={group.name}
								active={group.root === activeWorkspaceRoot}
								onSelect={onSelectWorkspace}
								onRename={onRenameWorkspace}
								onRemove={onRemoveWorkspace}
								onExpand={onExpandWorkspace}
								empty={!group.sessions.length && !hasMore?.(group.root)}
								initiallyExpanded={
									!onExpandWorkspace || group.root === activeWorkspaceRoot
								}
							>
								{group.sessions.map((session) => renderSession(session))}
								{hasMore?.(group.root) ? (
									<Button
										variant="ghost"
										size="sm"
										className="desktopLoadMore"
										onClick={() => onLoadMore?.(group.root)}
									>
										Load more
									</Button>
								) : null}
							</SidebarWorkspaceGroup>
						))}
						{!groups.length ? (
							<div className="desktopSidebarEmpty">
								<p>Open a folder to start a conversation.</p>
								<Button
									variant="outline"
									size="sm"
									disabled={pending}
									onClick={() =>
										void execute(async () => {
											if ((await onOpenWorkspace("customPath")) === false)
												throw new Error(
													"The workspace could not be opened. Try again.",
												);
										})
									}
								>
									Open folder…
								</Button>
							</div>
						) : null}
					</div>
				</section>
			</div>
			<footer className="thinSidebarFooter">
				<Tooltip>
					<TooltipTrigger
						render={
							<Button
								variant="ghost"
								aria-label="Settings"
								aria-current={activePage === "settings" ? "page" : undefined}
								onClick={() =>
									onNavigate ? onNavigate("settings") : onOpenResource("models")
								}
							/>
						}
					>
						<Settings size={16} aria-hidden="true" />
						<span>Settings</span>
					</TooltipTrigger>
					<TooltipContent side="right">Settings</TooltipContent>
				</Tooltip>
			</footer>
			<Dialog
				open={Boolean(dialog)}
				onOpenChange={(next, details) => {
					if (pending) details.cancel();
					else if (!next) setDialog(null);
				}}
			>
				{currentDialog ? (
					<DialogContent
						className="workbenchDialog"
						initialFocus={currentDialog.kind === "rename" ? input : undefined}
						finalFocus={() =>
							menuButtons.current.get(currentDialog.session.id) ??
							newChatButton.current
						}
						showCloseButton={!pending}
					>
						<DialogHeader>
							<DialogTitle>
								{currentDialog.kind === "rename"
									? "Rename conversation"
									: "Delete conversation?"}
							</DialogTitle>
							<DialogDescription>
								{currentDialog.kind === "rename"
									? "Choose a name that is easy to find in your workspace."
									: `“${currentDialog.session.title || "New chat"}” will be deleted. Running work in this conversation will be stopped.`}
							</DialogDescription>
						</DialogHeader>
						<form
							onSubmit={(event) => {
								event.preventDefault();
								if (currentDialog.kind === "rename" && draft.trim())
									void execute(
										() =>
											onRenameSession(currentDialog.session.id, draft.trim()),
										true,
									);
							}}
						>
							{currentDialog.kind === "rename" ? (
								<>
									<label htmlFor="conversation-name">Conversation name</label>
									<Input
										ref={input}
										id="conversation-name"
										value={draft}
										disabled={pending}
										onChange={(event) => setDraft(event.target.value)}
										onFocus={(event) => event.currentTarget.select()}
									/>
								</>
							) : null}
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
									onClick={() => setDialog(null)}
								>
									Cancel
								</Button>
								{currentDialog.kind === "rename" ? (
									<Button
										type="submit"
										disabled={
											pending ||
											!draft.trim() ||
											draft.trim() === currentDialog.session.title
										}
									>
										{pending ? "Saving…" : "Save"}
									</Button>
								) : (
									<Button
										variant="destructive"
										type="button"
										disabled={pending}
										onClick={() =>
											void execute(
												() => onDeleteSession(currentDialog.session.id),
												true,
											)
										}
									>
										{pending ? "Deleting…" : "Delete"}
									</Button>
								)}
							</DialogFooter>
						</form>
					</DialogContent>
				) : null}
			</Dialog>
		</aside>
	);
}
const formatRelativeTime = (updatedAt?: number): string => {
	if (!updatedAt || !Number.isFinite(updatedAt)) return "Just now";
	const minutes = Math.floor(Math.max(0, Date.now() - updatedAt) / 60_000);
	return minutes < 1
		? "Just now"
		: minutes < 60
			? `${minutes}m ago`
			: minutes < 1440
				? `${Math.floor(minutes / 60)}h ago`
				: `${Math.floor(minutes / 1440)}d ago`;
};
export default Sidebar;
