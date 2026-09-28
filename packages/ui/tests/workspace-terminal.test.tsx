import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, test, vi } from "vitest";
const mock = vi.hoisted(() => ({
	onData: null as null | ((s: string) => void),
	dispose: vi.fn(),
	write: vi.fn(),
	stop: vi.fn(),
}));
vi.mock("@xterm/xterm", () => ({
	Terminal: class {
		options = { disableStdin: false };
		cols = 80;
		rows = 24;
		loadAddon() {}
		open() {}
		focus() {}
		onData(fn: (s: string) => void) {
			mock.onData = fn;
			return { dispose() {} };
		}
		onBinary() {
			return { dispose() {} };
		}
		write(_b: Uint8Array, done: () => void) {
			mock.onData?.("\x1b[1;1R");
			done();
		}
		dispose() {
			mock.dispose();
		}
		reset() {}
		writeln() {}
	},
}));
vi.mock("@xterm/addon-fit", () => ({
	FitAddon: class {
		fit() {}
	},
}));
vi.mock("../src/lib/terminalBridge", () => ({
	readTerminal: vi.fn(async () => ({
		terminal: {
			state: "running",
			exitCode: null,
			error: null,
			outputComplete: false,
		},
		chunks: [{ dataBase64: "G1s2bg==" }],
		nextCursor: "1",
		gap: false,
		hasMore: false,
	})),
	resizeTerminal: vi.fn(async () => {}),
	writeTerminal: mock.write,
	stopTerminal: mock.stop,
}));
import WorkspaceTerminal from "../src/components/WorkspaceTerminal";
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
test("initial cursor query is answered and closing the view does not stop the terminal", async () => {
	vi.stubGlobal(
		"ResizeObserver",
		class {
			observe() {}
			disconnect() {}
		},
	);
	let view!: ReactTestRenderer;
	mock.write.mockResolvedValue({ accepted: true });
	await act(async () => {
		view = create(
			<WorkspaceTerminal
				target={{ terminalId: "t", serviceInstanceId: "s" }}
				active
			/>,
			{
				createNodeMock: () => ({
					addEventListener() {},
					removeEventListener() {},
					clientWidth: 800,
					clientHeight: 600,
				}),
			},
		);
	});
	expect(mock.write).toHaveBeenCalledWith(
		{ terminalId: "t", serviceInstanceId: "s" },
		new TextEncoder().encode("\x1b[1;1R"),
	);
	await act(async () => view.unmount());
	expect(mock.stop).not.toHaveBeenCalled();
	expect(mock.dispose).toHaveBeenCalled();
	vi.unstubAllGlobals();
});

test("large paste is written serially in bounded pieces without truncation", async () => {
	vi.stubGlobal(
		"ResizeObserver",
		class {
			observe() {}
			disconnect() {}
		},
	);
	let view!: ReactTestRenderer;
	mock.write.mockReset().mockResolvedValue({ accepted: true });
	await act(async () => {
		view = create(
			<WorkspaceTerminal
				target={{ terminalId: "t", serviceInstanceId: "s" }}
				active
			/>,
			{
				createNodeMock: () => ({
					addEventListener() {},
					removeEventListener() {},
					clientWidth: 800,
					clientHeight: 600,
				}),
			},
		);
	});
	mock.write.mockClear();
	const paste = "你好".repeat(40000);
	await act(async () => {
		mock.onData?.(paste);
	});
	const chunks = mock.write.mock.calls.map((call) => call[1] as Uint8Array);
	expect(chunks.length).toBeGreaterThan(1);
	expect(chunks.every((c) => c.length <= 8192)).toBe(true);
	const bytes = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0));
	let offset = 0;
	for (const c of chunks) {
		bytes.set(c, offset);
		offset += c.length;
	}
	expect(new TextDecoder().decode(bytes)).toBe(paste);
	await act(async () => view.unmount());
	vi.unstubAllGlobals();
});

test("terminal writes apply backpressure and never resend an uncertain chunk", async () => {
	vi.stubGlobal(
		"ResizeObserver",
		class {
			observe() {}
			disconnect() {}
		},
	);
	let view!: ReactTestRenderer;
	mock.write.mockReset().mockResolvedValue({ accepted: true });
	await act(async () => {
		view = create(
			<WorkspaceTerminal
				target={{ terminalId: "t", serviceInstanceId: "s" }}
				active
			/>,
			{
				createNodeMock: () => ({
					addEventListener() {},
					removeEventListener() {},
					clientWidth: 800,
					clientHeight: 600,
				}),
			},
		);
	});
	let reject!: (e: Error) => void;
	mock.write.mockReset().mockImplementation(
		() =>
			new Promise((_, r) => {
				reject = r;
			}),
	);
	await act(async () => {
		mock.onData?.("x".repeat(70000));
	});
	expect(mock.write).toHaveBeenCalledTimes(1);
	expect(mock.write.mock.calls[0][1]).toHaveLength(8192);
	await act(async () => {
		reject(new Error("unknown outcome"));
	});
	expect(mock.write).toHaveBeenCalledTimes(1);
	await act(async () => view.unmount());
	vi.unstubAllGlobals();
});
