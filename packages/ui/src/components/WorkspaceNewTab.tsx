import { useEffect, useRef, useState } from "react";
import { FolderOpen, FileDiff, TerminalSquare, LoaderCircle, ChevronDown } from "lucide-react";
import { Button } from "./ui/button";
import {
	createTerminal,
	listTerminals,
	type TerminalSnapshot,
	type TerminalList,
} from "../lib/terminalBridge";

type Props = {
	root: string | null;
	sessionId: string | null;
	active: boolean;
	onFiles: () => void;
	onReview: () => void;
	onTerminal: (terminal: TerminalSnapshot, serviceInstanceId: string, title?: string) => void;
};

export function WorkspaceNewTab({ root, sessionId, active, onFiles, onReview, onTerminal }: Props) {
	const scopeRef = useRef("");
	scopeRef.current = JSON.stringify([root, sessionId, active]);
	const pendingStart = useRef<Parameters<typeof createTerminal>[0] | null>(null);
	const activeStart = useRef<object | null>(null);
	const catalogOwner = useRef<object | null>(null);
	const [list, setList] = useState<TerminalList | null>(null);
	const [expanded, setExpanded] = useState(false);
	const [loading, setLoading] = useState(false);
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState("");
	useEffect(() => {
		// Leaving and returning to this launcher creates a new async owner.
		void active;
		activeStart.current = null;
		catalogOwner.current = null;
		if (
			pendingStart.current?.workspaceRoot !== root ||
			pendingStart.current?.sessionId !== sessionId
		)
			pendingStart.current = null;
		setList(null);
		setBusy(false);
		setLoading(false);
		setError("");
		setExpanded(false);
		return () => {
			activeStart.current = null;
			catalogOwner.current = null;
		};
	}, [root, sessionId, active]);

	const start = async () => {
		if (!active || !root || busy || activeStart.current) return;
		const scope = scopeRef.current;
		const owner = {};
		activeStart.current = owner;
		const isCurrent = () => scopeRef.current === scope && activeStart.current === owner;
		setBusy(true);
		setError("");
		try {
			const catalog = await listTerminals(root);
			if (!isCurrent()) return;
			setList(catalog);
			if (
				!pendingStart.current ||
				pendingStart.current.serviceInstanceId !== catalog.serviceInstanceId
			) {
				pendingStart.current = {
					operationId: crypto.randomUUID(),
					serviceInstanceId: catalog.serviceInstanceId,
					workspaceRoot: root,
					sessionId,
					shell: null,
					cols: 80,
					rows: 24,
				};
			}
			const terminal = await createTerminal(pendingStart.current);
			if (!isCurrent()) return;
			pendingStart.current = null;
			onTerminal(
				terminal,
				catalog.serviceInstanceId,
				(terminal.shell.split(/[\\/]/).pop() || "Terminal") + " " + (catalog.terminals.length + 1),
			);
		} catch (e) {
			if (isCurrent()) setError(String(e));
		} finally {
			if (isCurrent()) {
				activeStart.current = null;
				setBusy(false);
			}
		}
	};
	const refreshCatalog = async () => {
		if (!active || !root || loading) return;
		const scope = scopeRef.current;
		const owner = {};
		catalogOwner.current = owner;
		setExpanded(true);
		setLoading(true);
		setError("");
		try {
			const catalog = await listTerminals(root);
			if (scopeRef.current !== scope || catalogOwner.current !== owner) return;
			if (
				pendingStart.current &&
				pendingStart.current.serviceInstanceId !== catalog.serviceInstanceId
			) {
				activeStart.current = null;
				pendingStart.current = null;
				setBusy(false);
			}
			setList(catalog);
		} catch (e) {
			if (scopeRef.current === scope && catalogOwner.current === owner) setError(String(e));
		} finally {
			if (scopeRef.current === scope && catalogOwner.current === owner) setLoading(false);
		}
	};
	return (
		<div className="workspaceNewTab">
			<div className="workspaceNewTabContent">
				<h1>New tab</h1>
				<div className="workspaceNewTabTools" aria-label="Workspace tools">
					<Button variant="ghost" disabled={!root} onClick={onFiles}>
						<FolderOpen size={18} aria-hidden="true" />
						Files
					</Button>
					<Button variant="ghost" disabled={!root} onClick={onReview}>
						<FileDiff size={18} aria-hidden="true" />
						Review
					</Button>
					<Button variant="ghost" disabled={!root || busy} onClick={() => void start()}>
						{busy ? (
							<LoaderCircle className="workbenchSpinner" size={18} aria-hidden="true" />
						) : (
							<TerminalSquare size={18} aria-hidden="true" />
						)}
						{busy ? "Starting…" : "Terminal"}
					</Button>
				</div>
				{root ? (
					<div className="workspaceNewTabTerminals">
						<Button
							variant="ghost"
							size="sm"
							aria-expanded={expanded}
							onClick={() => (expanded ? setExpanded(false) : void refreshCatalog())}
						>
							<ChevronDown size={14} aria-hidden="true" />
							Existing terminals
						</Button>
						{expanded ? (
							<div className="workspaceExistingTerminals">
								{loading ? (
									<span role="status">Loading terminals…</span>
								) : list?.terminals.length ? (
									list.terminals.map((terminal, index) => (
										<Button
											variant="ghost"
											key={terminal.terminalId}
											onClick={() =>
												onTerminal(
													terminal,
													list.serviceInstanceId,
													(terminal.shell.split(/[\\/]/).pop() || "Terminal") + " " + (index + 1),
												)
											}
										>
											<TerminalSquare size={15} aria-hidden="true" />
											<span>
												{terminal.shell.split(/[\\/]/).pop()} {index + 1}
											</span>
											<small>{terminal.state === "exited" ? "Exited" : "Running"}</small>
										</Button>
									))
								) : !error ? (
									<span>No terminals</span>
								) : null}
							</div>
						) : null}
					</div>
				) : null}
				{error ? (
					<div className="workbenchInlineError" role="alert">
						<p>{error}</p>
						<Button
							variant="outline"
							size="sm"
							disabled={busy || loading}
							onClick={() => void (expanded ? refreshCatalog() : start())}
						>
							Retry
						</Button>
					</div>
				) : null}
			</div>
		</div>
	);
}
