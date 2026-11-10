import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { MoreHorizontal } from "lucide-react";
import { useResolvedTheme } from "../theme";
import { Button } from "./ui/button";
import {
	DropdownMenu,
	DropdownMenuTrigger,
	DropdownMenuContent,
	DropdownMenuItem,
} from "./ui/dropdown-menu";

function terminalAppearance(element: HTMLElement) {
	const doc = element.ownerDocument;
	const style = doc?.defaultView?.getComputedStyle(doc.documentElement);
	const color = (role: string) => style?.getPropertyValue(role).trim() || undefined;
	return {
		fontFamily: color("--font-mono") || '"Google Sans Code", monospace',
		theme: {
			background: color("--surface-color"),
			foreground: color("--on-surface"),
			cursor: color("--on-surface"),
			cursorAccent: color("--surface-color"),
			selectionBackground: color("--surface-container"),
			selectionForeground: color("--on-surface"),
			black: color("--on-surface"),
			brightBlack: color("--process-text"),
			red: color("--error"),
			brightRed: color("--error"),
			green: color("--success"),
			brightGreen: color("--success"),
			yellow: color("--warning"),
			brightYellow: color("--warning"),
			blue: color("--primary-color"),
			brightBlue: color("--primary-color"),
			magenta: color("--code-keyword"),
			brightMagenta: color("--code-keyword"),
			cyan: color("--terminal-cyan"),
			brightCyan: color("--terminal-cyan"),
			white: color("--on-surface"),
			brightWhite: color("--on-surface"),
		},
	};
}
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
	const resolvedTheme = useResolvedTheme();
	const terminal = useRef<Terminal | null>(null);
	const requestFit = useRef<(() => void) | null>(null);
	const container = useRef<HTMLDivElement>(null);
	const activeRef = useRef(active);
	activeRef.current = active;
	const [error, setError] = useState("");
	const [state, setState] = useState("running");
	const [exitCode, setExitCode] = useState<number | null>(null);
	const [revision, setRevision] = useState(0);
	const [fontReady, setFontReady] = useState(
		() => typeof document === "undefined" || !document.fonts?.load,
	);
	useEffect(() => {
		const fonts = container.current?.ownerDocument?.fonts;
		if (!fonts?.load) return;
		let owned = true;
		// xterm fixes its cell width when opened; fitting later does not remeasure
		// a fallback font. Load our bundled monospace face before that first measure.
		void fonts.load('14px "Google Sans Code"').then(
			() => {
				if (owned) setFontReady(true);
			},
			() => {
				if (owned) setFontReady(true);
			},
		);
		return () => {
			owned = false;
		};
	}, []);
	useEffect(() => {
		void revision; // Explicit reconnect keeps its own effect lifetime.
		if (!fontReady || !container.current) return;
		const target = { terminalId, serviceInstanceId };
		const element = container.current;
		const term = new Terminal({
			cursorBlink: true,
			scrollback: 2000,
			fontSize: 14,
			lineHeight: 1.35,
			...terminalAppearance(element),
			convertEol: false,
			allowProposedApi: false,
		});
		terminal.current = term;
		const fit = new FitAddon();
		term.loadAddon(fit);
		term.open(element);
		if (activeRef.current) term.focus();
		let disposed = false,
			blocked = false,
			rendering = false,
			ready = false,
			running = true,
			cursor = "0";
		let pendingInputs = 0;
		const blockUserInput = (event: Event) => {
			if (pendingInputs) {
				event.preventDefault();
				event.stopPropagation();
			}
		};
		for (const name of ["keydown", "beforeinput", "paste"])
			element.addEventListener(name, blockUserInput, true);
		let queue = Promise.resolve();
		let timer: ReturnType<typeof setTimeout>;
		let sizing: ReturnType<typeof setTimeout>;
		const fail = (e: unknown) => {
			blocked = true;
			if (!disposed) setError(String(e));
		};
		const input = (bytes: Uint8Array) => {
			if (blocked || (!ready && !rendering) || !running || disposed) return;
			// The current paste is retained once; only one bounded chunk is in
			// flight. Pause further user input until the writer catches up.
			pendingInputs++;
			if (term.textarea) term.textarea.readOnly = true;
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
					pendingInputs--;
					if (!disposed && !blocked && !pendingInputs && term.textarea)
						term.textarea.readOnly = false;
				});
		};
		const data = term.onData((value) => input(new TextEncoder().encode(value)));
		const binary = term.onBinary((value) => input(Uint8Array.from(value, (c) => c.charCodeAt(0))));
		const resize = () => {
			clearTimeout(sizing);
			sizing = setTimeout(() => {
				if (disposed || !activeRef.current || !element.clientWidth || !element.clientHeight) return;
				fit.fit();
				if (running)
					void resizeTerminal(target, Math.min(500, term.cols), Math.min(300, term.rows)).catch(
						fail,
					);
			}, 80);
		};
		requestFit.current = resize;
		const fonts = element.ownerDocument?.fonts;
		void fonts?.ready.then(() => {
			if (!disposed) resize();
		});
		fonts?.addEventListener("loadingdone", resize);
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
					const bytes = Uint8Array.from(atob(chunk.dataBase64), (c) => c.charCodeAt(0));
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
				if (page.terminal.state === "exited" && page.terminal.outputComplete) return;
				timer = setTimeout(() => void poll(), page.hasMore ? 0 : 200);
			} catch (e) {
				fail(e);
			}
		};
		setError("");
		void poll();
		return () => {
			disposed = true;
			for (const name of ["keydown", "beforeinput", "paste"])
				element.removeEventListener(name, blockUserInput, true);
			clearTimeout(timer);
			clearTimeout(sizing);
			observer.disconnect();
			fonts?.removeEventListener("loadingdone", resize);
			requestFit.current = null;
			terminal.current = null;
			data.dispose();
			binary.dispose();
			term.dispose();
		};
	}, [terminalId, serviceInstanceId, revision, fontReady]);
	useEffect(() => {
		void resolvedTheme;
		if (terminal.current && container.current)
			Object.assign(terminal.current.options, terminalAppearance(container.current));
		if (active) requestFit.current?.();
	}, [resolvedTheme, active]);
	return (
		<div className="workspaceTerminal">
			{state !== "exited" ? (
				<div className="workspaceTerminalActions">
					<DropdownMenu>
						<DropdownMenuTrigger
							render={<Button variant="ghost" size="icon-sm" aria-label="Terminal actions" />}
						>
							<MoreHorizontal size={16} aria-hidden="true" />
						</DropdownMenuTrigger>
						<DropdownMenuContent align="end">
							<DropdownMenuItem
								disabled={state === "stopping"}
								onClick={() =>
									void stopTerminal(target)
										.then(() => setState("stopping"))
										.catch((e) => setError(String(e)))
								}
							>
								End terminal
							</DropdownMenuItem>
						</DropdownMenuContent>
					</DropdownMenu>
				</div>
			) : (
				<div className="workspaceTerminalExit" role="status">
					Terminal exited{exitCode !== null ? " · Exit " + exitCode : ""}
				</div>
			)}
			{error ? (
				<div className="workspaceTerminalError" role="status">
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
