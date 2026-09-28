import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import {
	readTerminal,
	resizeTerminal,
	stopTerminal,
	writeTerminal,
	type TerminalTarget,
} from "../lib/terminalBridge";
export default function WorkspaceTerminal({
	target,
	active,
}: {
	target: TerminalTarget;
	active: boolean;
}) {
	const { terminalId, serviceInstanceId } = target;
	const container = useRef<HTMLDivElement>(null);
	const activeRef = useRef(active);
	activeRef.current = active;
	const [error, setError] = useState("");
	const [state, setState] = useState("running");
	const [exitCode, setExitCode] = useState<number | null>(null);
	const [revision, setRevision] = useState(0);
	useEffect(() => {
		if (!container.current) return;
		const target = { terminalId, serviceInstanceId };
		const element = container.current;
		const term = new Terminal({
			cursorBlink: true,
			scrollback: 2000,
			fontSize: 13,
			convertEol: false,
			allowProposedApi: false,
			theme: { background: "#181818", foreground: "#dedede" },
		});
		const fit = new FitAddon();
		term.loadAddon(fit);
		term.open(element);
		if (activeRef.current) term.focus();
		let disposed = false,
			blocked = false,
			rendering = false,
			ready = false,
			running = true,
			cursor = "0",
			queued = 0;
		let queue = Promise.resolve();
		let timer: ReturnType<typeof setTimeout>;
		let sizing: ReturnType<typeof setTimeout>;
		const fail = (e: unknown) => {
			blocked = true;
			if (!disposed) setError(String(e));
		};
		const input = (bytes: Uint8Array) => {
			if (blocked || (!ready && !rendering) || !running || disposed) return;
			if (queued + bytes.length > 65536) {
				fail("Input queue full. Input was not resent.");
				return;
			}
			queued += bytes.length;
			queue = queue
				.then(async () => {
					if (disposed || blocked) return;
					for (let i = 0; i < bytes.length; i += 8192) {
						if (disposed || blocked) return;
						await writeTerminal(target, bytes.slice(i, i + 8192));
					}
				})
				.catch(fail)
				.finally(() => {
					queued -= bytes.length;
				});
		};
		const data = term.onData((value) => input(new TextEncoder().encode(value)));
		const binary = term.onBinary((value) =>
			input(Uint8Array.from(value, (c) => c.charCodeAt(0))),
		);
		const resize = () => {
			clearTimeout(sizing);
			sizing = setTimeout(() => {
				if (
					disposed ||
					!activeRef.current ||
					!element.clientWidth ||
					!element.clientHeight
				)
					return;
				fit.fit();
				if (running)
					void resizeTerminal(
						target,
						Math.min(500, term.cols),
						Math.min(300, term.rows),
					).catch(fail);
			}, 80);
		};
		const observer = new ResizeObserver(resize);
		observer.observe(element);
		resize();
		const poll = async () => {
			try {
				const page = await readTerminal(target, cursor);
				if (disposed) return;
				if (page.gap) {
					term.reset();
					term.writeln("[Earlier terminal output is no longer available]");
				}
				for (const chunk of page.chunks) {
					const bytes = Uint8Array.from(atob(chunk.dataBase64), (c) =>
						c.charCodeAt(0),
					);
					rendering = true;
					await new Promise<void>((resolve) => term.write(bytes, resolve));
					rendering = false;
					if (disposed) return;
				}
				cursor = page.nextCursor;
				running = page.terminal.state === "running";
				ready = !page.hasMore;
				setState(page.terminal.state);
				setExitCode(page.terminal.exitCode);
				if (page.terminal.error) fail(page.terminal.error);
				if (page.terminal.state === "exited" && page.terminal.outputComplete)
					return;
				timer = setTimeout(() => void poll(), page.hasMore ? 0 : 200);
			} catch (e) {
				fail(e);
			}
		};
		setError("");
		void poll();
		return () => {
			disposed = true;
			clearTimeout(timer);
			clearTimeout(sizing);
			observer.disconnect();
			data.dispose();
			binary.dispose();
			term.dispose();
		};
	}, [terminalId, serviceInstanceId, revision]);
	return (
		<div className="workspaceTerminal">
			<header>
				<span>
					{state}
					{exitCode !== null ? ` · Exit ${exitCode}` : ""}
				</span>
				{state !== "exited" ? (
					<button
						type="button"
						onClick={() =>
							void stopTerminal(target)
								.then(() => setState("stopping"))
								.catch((e) => setError(String(e)))
						}
					>
						End terminal
					</button>
				) : null}
			</header>
			{error ? (
				<div role="status">
					{error}
					<button type="button" onClick={() => setRevision((v) => v + 1)}>
						Reconnect
					</button>
				</div>
			) : null}
			<div className="workspaceTerminalCanvas" ref={container} />
		</div>
	);
}
