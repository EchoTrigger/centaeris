import { useEffect, useRef, useState } from "react";
import { Plus } from "lucide-react";
import {
	createTerminal,
	listTerminals,
	type TerminalSnapshot,
	type TerminalList,
} from "../lib/terminalBridge";
export function WorkspaceAddMenu({
	root,
	sessionId,
	onFiles,
	onReview,
	onTerminal,
}: {
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
	const [list, setList] = useState<TerminalList | null>(null);
	const [error, setError] = useState("");
	const [busy, setBusy] = useState(false);
	const ref = useRef<HTMLDivElement>(null);
	useEffect(() => {
		if (!open) return;
		let disposed = false;
		setList(null);
		setError("");
		void listTerminals(root)
			.then((v) => {
				if (!disposed) setList(v);
			})
			.catch((e) => {
				if (!disposed) setError(String(e));
			});
		const close = (e: MouseEvent) => {
			if (!ref.current?.contains(e.target as Node)) setOpen(false);
		};
		const key = (e: KeyboardEvent) => {
			if (e.key === "Escape") setOpen(false);
		};
		document.addEventListener("mousedown", close);
		document.addEventListener("keydown", key);
		return () => {
			disposed = true;
			document.removeEventListener("mousedown", close);
			document.removeEventListener("keydown", key);
		};
	}, [open, root]);
	const start = async () => {
		if (!list || busy) return;
		const scope = scopeRef.current;
		setBusy(true);
		setError("");
		try {
			if (
				!pendingStart.current ||
				pendingStart.current.workspaceRoot !== root ||
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
			pendingStart.current = null;
			if (scopeRef.current !== scope) return;
			onTerminal(
				terminal,
				list.serviceInstanceId,
				`${terminal.shell.split(/[\\/]/).pop()} ${list.terminals.length + 1}`,
			);
			setOpen(false);
		} catch (e) {
			setError(String(e));
		} finally {
			setBusy(false);
		}
	};
	return (
		<div ref={ref} className="workspaceAddMenu">
			<button
				type="button"
				aria-label="Add workspace tab"
				aria-expanded={open}
				onClick={() => setOpen((v) => !v)}
			>
				<Plus size={16} />
			</button>
			{open ? (
				<div className="workspaceAddChoices">
					<button
						type="button"
						onClick={() => {
							onFiles();
							setOpen(false);
						}}
					>
						Files
					</button>
					<button
						type="button"
						onClick={() => {
							onReview();
							setOpen(false);
						}}
					>
						Review
					</button>
					<button
						type="button"
						disabled={busy || !list}
						onClick={() => void start()}
					>
						{busy ? "Starting…" : "Terminal"}
					</button>
					{list?.terminals.length ? (
						<>
							<small>Existing terminals</small>
							{list.terminals.map((terminal, index) => (
								<button
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
									{terminal.shell.split(/[\\/]/).pop()} {index + 1} ·{" "}
									{terminal.state}
								</button>
							))}
						</>
					) : null}
					{error ? <p role="status">{error}</p> : null}
				</div>
			) : null}
		</div>
	);
}
