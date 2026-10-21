import { Button } from "./ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "./ui/popover";
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
	const activeStart = useRef<object | null>(null);
	useEffect(() => {
		// A departed view cannot own the new view's retry, error or busy state.
		if (pendingStart.current?.workspaceRoot !== root || pendingStart.current?.sessionId !== sessionId) {
			activeStart.current = null;
			pendingStart.current = null;
			setError("");
			setBusy(false);
		}
		return () => { activeStart.current = null; };
	}, [root, sessionId]);
	useEffect(() => {
		if (!open) return;
		let disposed = false;
		setList(null);
		setError("");
		void listTerminals(root)
			.then((v) => {
				if (disposed) return;
				if (pendingStart.current && pendingStart.current.serviceInstanceId !== v.serviceInstanceId) {
					activeStart.current = null;
					pendingStart.current = null;
					setBusy(false);
				}
				setList(v);
			})
			.catch((e) => {
				if (!disposed) setError(String(e));
			});
		return () => { disposed = true; };
	}, [open, root]);
	const start = async () => {
		if (!list || busy || activeStart.current) return;
		const scope = scopeRef.current;
		const owner = {};
		activeStart.current = owner;
		const isCurrent = () => scopeRef.current === scope && activeStart.current === owner;
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
		<Popover open={open} onOpenChange={setOpen}><div className="workspaceAddMenu">
			<PopoverTrigger render={<Button variant="ghost" size="icon" aria-label="Add workspace tab" />}>
				<Plus size={16} />
			</PopoverTrigger>
			{open ? (
				<PopoverContent align="end" className="workspaceAddChoices">
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
				</PopoverContent>
			) : null}
		</div></Popover>
	);
}
