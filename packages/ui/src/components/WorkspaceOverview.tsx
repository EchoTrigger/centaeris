import { useEffect, useState } from "react";
import {
	FileDiff,
	FolderOpen,
	GitBranch,
	Layers,
	Bot,
	ListCollapse,
} from "lucide-react";
import {
	getWorkspaceGitStatus,
	type WorkspaceGitStatusResponse,
} from "../lib/workspaceBridge";
import { listProcesses } from "../lib/processBridge";
import type { UiSession } from "../types/ui";
import { Button } from "./ui/button";
import {
	Popover,
	PopoverTrigger,
	PopoverContent,
	PopoverTitle,
	PopoverDescription,
} from "./ui/popover";
import "../styles/desktop-workbench.css";

export function WorkspaceOverview({
	name,
	root,
	agents,
	onFiles,
	onReview,
	onAgent,
	onTasks,
	sessionId,
}: {
	sessionId?: string | null;
	onTasks?: () => void;
	name: string;
	root: string | null;
	agents: UiSession[];
	onFiles: () => void;
	onReview: () => void;
	onAgent: (id: string, title: string) => void;
}) {
	const [open, setOpen] = useState(false);
	const [gitStatus, setGitStatus] = useState<WorkspaceGitStatusResponse | null>(
		null,
	);
	const [gitError, setGitError] = useState("");
	useEffect(() => {
		if (!open || !root) return;
		let cancelled = false;
		setGitStatus(null);
		setGitError("");
		void getWorkspaceGitStatus(root)
			.then((status) => {
				if (!cancelled) setGitStatus(status);
			})
			.catch((error: unknown) => {
				if (!cancelled) setGitError(String(error));
			});
		return () => {
			cancelled = true;
		};
	}, [open, root]);
	const [taskCount, setTaskCount] = useState({ sessionId: "", count: 0 });
	useEffect(() => {
		if (!open || !sessionId || !onTasks) return;
		let disposed = false;
		let timer: ReturnType<typeof setTimeout>;
		const refresh = async () => {
			try {
				const result = await listProcesses(sessionId);
				if (!disposed)
					setTaskCount({ sessionId, count: result.processes.length });
			} catch {
				if (!disposed) setTaskCount({ sessionId, count: 0 });
			} finally {
				if (!disposed) timer = setTimeout(refresh, 1500);
			}
		};
		void refresh();
		return () => {
			disposed = true;
			clearTimeout(timer);
		};
	}, [open, sessionId, onTasks]);
	const activate = (action: () => void) => {
		setOpen(false);
		action();
	};
	return (
		<Popover open={open} onOpenChange={setOpen}>
			<PopoverTrigger
				render={
					<Button
						variant="ghost"
						size="sm"
						className="desktopResourcesTrigger"
						aria-label="Workspace overview"
					/>
				}
			>
				<Layers size={15} aria-hidden="true" />
				<span>Resources</span>
			</PopoverTrigger>
			<PopoverContent
				align="end"
				className="desktopResourcesPopover"
				aria-label="Workspace resources"
			>
				<div className="desktopResourceHeading">
					<PopoverTitle>{name}</PopoverTitle>
					<PopoverDescription title={root ?? undefined}>
						{root || "No workspace selected"}
					</PopoverDescription>
				</div>
				<div className="desktopResourceLinks">
					<Button
						variant="ghost"
						disabled={!root}
						aria-label="Browse files"
						onClick={() => activate(onFiles)}
					>
						<FolderOpen size={16} aria-hidden="true" />
						<span>
							Files<small>Browse workspace files</small>
						</span>
					</Button>
					<Button
						variant="ghost"
						disabled={!root}
						onClick={() => activate(onReview)}
					>
						<FileDiff size={16} aria-hidden="true" />
						<span>
							Review changes<small>Inspect changes in your workspace</small>
						</span>
						{gitStatus?.isGitRepository ? (
							<span className="workspaceOverviewStats">
								<span>+{gitStatus.totalAdded}</span>
								<span>−{gitStatus.totalRemoved}</span>
							</span>
						) : null}
					</Button>
					{onTasks &&
					taskCount.sessionId === sessionId &&
					taskCount.count > 0 ? (
						<Button
							variant="ghost"
							aria-label="Task"
							onClick={() => activate(onTasks)}
						>
							<ListCollapse size={16} aria-hidden="true" />
							<span>
								Tasks<small>Running and completed processes</small>
							</span>
							<span className="desktopResourceCount">{taskCount.count}</span>
						</Button>
					) : null}
				</div>
				{root ? (
					<div className="desktopResourceGit">
						{gitStatus?.isGitRepository ? (
							<>
								<GitBranch size={14} aria-hidden="true" />
								<span>{gitStatus.branch || "Detached HEAD"}</span>
							</>
						) : (
							<span>
								{gitError ||
									(gitStatus ? "Not a Git repository" : "Loading Git status…")}
							</span>
						)}
					</div>
				) : null}
				{agents.length ? (
					<div className="desktopResourceAgents">
						<p>
							Agents <span>{agents.length}</span>
						</p>
						{agents.map((agent) => (
							<Button
								variant="ghost"
								key={agent.id}
								onClick={() => activate(() => onAgent(agent.id, agent.title))}
							>
								<Bot size={16} aria-hidden="true" />
								<span>{agent.title}</span>
							</Button>
						))}
					</div>
				) : null}
			</PopoverContent>
		</Popover>
	);
}
