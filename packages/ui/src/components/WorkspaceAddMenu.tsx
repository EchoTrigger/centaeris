import { Button } from "./ui/button";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
	PopoverTitle,
} from "./ui/popover";
import { useEffect, useRef, useState } from "react";
import {
	Plus,
	ChevronDown,
	FolderOpen,
	FileDiff,
	TerminalSquare,
	Loader2,
} from "lucide-react";
import {
	createTerminal,
	listTerminals,
	type TerminalSnapshot,
	type TerminalList,
} from "../lib/terminalBridge";
import "../styles/desktop-workbench.css";
export function WorkspaceAddMenu({
	root,
	sessionId,
	onFiles,
	onReview,
	onTerminal,
	triggerLabel,
}: {
	triggerLabel?: string;
	root: string;
	sessionId: string | null;
	onFiles: () => void;
	onReview: () => void;
	onTerminal: (
		terminal: TerminalSnapshot,
		serviceInstanceId: string,
		title?: string,
	) => void;
}) {
	const scopeRef = useRef("");
	scopeRef.current = `${root}\0${sessionId}`;
	const pendingStart = useRef<Parameters<typeof createTerminal>[0] | null>(
		null,
	);
	const [open, setOpen] = useState(false);
	const [listRevision, setListRevision] = useState(0);
	const [list, setList] = useState<TerminalList | null>(null);
	const [error, setError] = useState("");
	const [busy, setBusy] = useState(false);
	const activeStart = useRef<object | null>(null);
	useEffect(() => {
		// A departed view cannot own the new view's retry, error or busy state.
		if (
			pendingStart.current?.workspaceRoot !== root ||
			pendingStart.current?.sessionId !== sessionId
		) {
			activeStart.current = null;
			pendingStart.current = null;
			setError("");
			setBusy(false);
		}
		return () => {
			activeStart.current = null;
		};
	}, [root, sessionId]);
	// biome-ignore lint/correctness/useExhaustiveDependencies: listRevision is the explicit retry trigger after a failed terminal catalog request.
	useEffect(() => {
		if (!open) return;
		let disposed = false;
		setList(null);
		setError("");
		void listTerminals(root)
			.then((v) => {
				if (disposed) return;
				if (
					pendingStart.current &&
					pendingStart.current.serviceInstanceId !== v.serviceInstanceId
				) {
					activeStart.current = null;
					pendingStart.current = null;
					setBusy(false);
				}
				setList(v);
			})
			.catch((e) => {
				if (!disposed) setError(String(e));
			});
		return () => {
			disposed = true;
		};
	}, [open, root, listRevision]);
	const start = async () => {
		if (!list || busy || activeStart.current) return;
		const scope = scopeRef.current;
		const owner = {};
		activeStart.current = owner;
		const isCurrent = () =>
			scopeRef.current === scope && activeStart.current === owner;
		setBusy(true);
		setError("");
		try {
			if (
				!pendingStart.current ||
				pendingStart.current.workspaceRoot !== root ||
				pendingStart.current.sessionId !== sessionId ||
				pendingStart.current.serviceInstanceId !== list.serviceInstanceId
			)
				pendingStart.current = {
					operationId: crypto.randomUUID(),
					serviceInstanceId: list.serviceInstanceId,
					workspaceRoot: root,
					sessionId,
					shell: null,
					cols: 80,
					rows: 24,
				};
			const terminal = await createTerminal(pendingStart.current);
			if (!isCurrent()) return;
			pendingStart.current = null;
			onTerminal(
				terminal,
				list.serviceInstanceId,
				`${terminal.shell.split(/[\\/]/).pop()} ${list.terminals.length + 1}`,
			);
			setOpen(false);
		} catch (e) {
			if (isCurrent()) setError(String(e));
		} finally {
			if (isCurrent()) {
				activeStart.current = null;
				setBusy(false);
			}
		}
	};
	return (
		<Popover open={open} onOpenChange={setOpen}>
			<div className="workspaceAddMenu">
				<PopoverTrigger
					render={
						<Button
							variant="ghost"
							size={triggerLabel ? "sm" : "icon"}
							className="desktopTerminalTrigger"
							aria-label={
								triggerLabel ? "Terminal options" : "Add workspace tab"
							}
						/>
					}
				>
					{triggerLabel ? (
						<>
							<TerminalSquare size={15} aria-hidden="true" />
							<span>{triggerLabel}</span>
							<ChevronDown size={12} aria-hidden="true" />
						</>
					) : (
						<Plus size={16} aria-hidden="true" />
					)}
				</PopoverTrigger>
				{open ? (
					<PopoverContent align="end" className="desktopViewChoices">
						<PopoverTitle>Workspace views</PopoverTitle>
						<Button
							variant="ghost"
							type="button"
							onClick={() => {
								onFiles();
								setOpen(false);
							}}
						>
							<FolderOpen size={16} aria-hidden="true" />
							Files
						</Button>
						<Button
							variant="ghost"
							type="button"
							onClick={() => {
								onReview();
								setOpen(false);
							}}
						>
							<FileDiff size={16} aria-hidden="true" />
							Review
						</Button>
						<Button
							variant="ghost"
							type="button"
							disabled={busy || !list}
							onClick={() => void start()}
						>
							{busy ? (
								<Loader2
									size={16}
									className="workbenchSpinner"
									aria-hidden="true"
								/>
							) : (
								<TerminalSquare size={16} aria-hidden="true" />
							)}
							{busy ? "Starting…" : "Terminal"}
						</Button>
						{list?.terminals.length ? (
							<>
								<small className="desktopViewSectionLabel">
									Existing terminals
								</small>
								{list.terminals.map((terminal, index) => (
									<Button
										variant="ghost"
										type="button"
										key={terminal.terminalId}
										onClick={() => {
											onTerminal(
												terminal,
												list.serviceInstanceId,
												`${terminal.shell.split(/[\\/]/).pop()} ${index + 1}`,
											);
											setOpen(false);
										}}
									>
										<TerminalSquare size={16} aria-hidden="true" />
										{terminal.shell.split(/[\\/]/).pop()} {index + 1} ·{" "}
										{terminal.state}
									</Button>
								))}
							</>
						) : null}
						{!list && !error ? (
							<p className="desktopViewLoading" role="status">
								Loading terminals…
							</p>
						) : null}
						{error ? (
							<div className="workbenchInlineError" role="alert">
								<p>{error}</p>
								{!list ? (
									<Button
										variant="outline"
										size="sm"
										onClick={() => setListRevision((value) => value + 1)}
									>
										Retry
									</Button>
								) : null}
							</div>
						) : null}
					</PopoverContent>
				) : null}
			</div>
		</Popover>
	);
}
