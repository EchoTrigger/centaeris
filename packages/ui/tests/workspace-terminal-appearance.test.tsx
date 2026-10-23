// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
const mock = vi.hoisted(() => ({
	terms: [] as { options: Record<string, unknown> }[],
	fit: vi.fn(),
	stop: vi.fn(),
}));
vi.mock("@xterm/xterm", () => ({
	Terminal: class {
		options: Record<string, unknown>;
		cols = 80;
		rows = 24;
		constructor(options: Record<string, unknown>) {
			this.options = options;
			mock.terms.push(this);
		}
		open() {}
		focus() {}
		loadAddon() {}
		dispose() {}
		reset() {}
		writeln() {}
		onData() {
			return { dispose() {} };
		}
		onBinary() {
			return { dispose() {} };
		}
		write(_bytes: Uint8Array, done: () => void) {
			done();
		}
	},
}));
vi.mock("@xterm/addon-fit", () => ({
	FitAddon: class {
		fit = mock.fit;
	},
}));
vi.mock("../src/lib/terminalBridge", () => ({
	readTerminal: vi.fn(async () => ({
		terminal: { state: "running", exitCode: null, error: null, outputComplete: false },
		chunks: [],
		nextCursor: "0",
		gap: false,
		hasMore: false,
	})),
	resizeTerminal: vi.fn(async () => {}),
	writeTerminal: vi.fn(),
	stopTerminal: mock.stop,
}));
import WorkspaceTerminal from "../src/components/WorkspaceTerminal";
beforeEach(() => {
	mock.terms.length = 0;
	mock.stop.mockReset().mockResolvedValue({});
	document.documentElement.dataset.theme = "light";
	document.documentElement.style.setProperty("--surface-color", "#fcfdfe");
	document.documentElement.style.setProperty("--on-surface", "#26313e");
	vi.stubGlobal(
		"ResizeObserver",
		class {
			observe() {}
			disconnect() {}
		},
	);
});
afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
	document.documentElement.removeAttribute("style");
});
test("terminal follows application theme without replacing its connection or buffer", async () => {
	const view = render(
		<WorkspaceTerminal target={{ terminalId: "pty", serviceInstanceId: "service" }} active />,
	);
	expect(mock.terms[0].options.theme).toMatchObject({
		background: "#fcfdfe",
		foreground: "#26313e",
	});
	await act(async () => {
		document.documentElement.dataset.theme = "dark";
		document.documentElement.style.setProperty("--surface-color", "#191919");
		document.documentElement.style.setProperty("--on-surface", "#ededed");
		window.dispatchEvent(new Event("centaeris-theme-applied"));
	});
	await waitFor(() =>
		expect(mock.terms[0].options.theme).toMatchObject({
			background: "#191919",
			foreground: "#ededed",
		}),
	);
	expect(mock.terms).toHaveLength(1);
	view.unmount();
	expect(mock.stop).not.toHaveBeenCalled();
});
test("ending a terminal is an explicit menu action", async () => {
	const user = userEvent.setup();
	render(<WorkspaceTerminal target={{ terminalId: "pty", serviceInstanceId: "service" }} active />);
	await user.click(screen.getByRole("button", { name: "Terminal actions" }));
	await user.click(await screen.findByRole("menuitem", { name: "End terminal" }));
	expect(mock.stop).toHaveBeenCalledExactlyOnceWith({
		terminalId: "pty",
		serviceInstanceId: "service",
	});
});

test("terminal waits for its code font before measuring the character grid", async () => {
	let ready!: () => void;
	const load = vi.fn(
		() =>
			new Promise<void>((resolve) => {
				ready = resolve;
			}),
	);
	Object.defineProperty(document, "fonts", {
		configurable: true,
		value: { load, ready: Promise.resolve(), addEventListener() {}, removeEventListener() {} },
	});
	try {
		render(
			<WorkspaceTerminal target={{ terminalId: "pty", serviceInstanceId: "service" }} active />,
		);
		expect(load).toHaveBeenCalled();
		expect(mock.terms).toHaveLength(0);
		await act(async () => ready());
		await waitFor(() => expect(mock.terms).toHaveLength(1));
	} finally {
		Reflect.deleteProperty(document, "fonts");
	}
});
