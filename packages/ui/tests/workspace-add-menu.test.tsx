// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { WorkspaceAddMenu } from "../src/components/WorkspaceAddMenu";
import type {
	createTerminal,
	TerminalSnapshot,
} from "../src/lib/terminalBridge";

type StartRequest = Parameters<typeof createTerminal>[0];
const host = vi.hoisted(() => ({
	invokeHost: vi.fn(),
	list: vi.fn(),
	start: vi.fn(),
}));
vi.mock("../src/host/hostBridge", () => ({ invokeHost: host.invokeHost }));
const terminal = (
	sessionId: string | null = null,
	root = "/root",
): TerminalSnapshot => ({
	terminalId: `terminal:${sessionId}`,
	workspaceRoot: root,
	sessionId,
	shell: "pwsh",
	state: "running",
	exitCode: null,
	outputComplete: false,
	error: null,
});
beforeEach(() => {
	vi.resetAllMocks();
	host.list.mockResolvedValue({
		serviceInstanceId: "service-a",
		terminals: [],
	});
	host.invokeHost.mockImplementation(
		async (
			command: string,
			payload: {
				request: {
					action: string;
					workspaceRoot?: string;
					request?: StartRequest;
				};
			},
		) => {
			if (command !== "terminal_manage")
				throw new Error(`Unexpected host command ${command}`);
			if (payload.request.action === "list")
				return host.list(payload.request.workspaceRoot);
			if (payload.request.action === "start")
				return host.start(payload.request.request);
			throw new Error(`Unexpected terminal action ${payload.request.action}`);
		},
	);
});
afterEach(cleanup);
const choices = (
	root: string,
	sessionId: string | null,
	onTerminal = vi.fn(),
) => (
	<WorkspaceAddMenu
		root={root}
		sessionId={sessionId}
		onFiles={() => {}}
		onReview={() => {}}
		onTerminal={onTerminal}
	/>
);
async function openChoices(user: ReturnType<typeof userEvent.setup>) {
	await user.click(screen.getByRole("button", { name: "Add workspace tab" }));
	await waitFor(() =>
		expect(
			(screen.getByRole("button", { name: "Terminal" }) as HTMLButtonElement)
				.disabled,
		).toBe(false),
	);
}
function pendingStart() {
	let resolve!: (value: TerminalSnapshot) => void;
	let reject!: (error: Error) => void;
	const promise = new Promise<TerminalSnapshot>((finish, fail) => {
		resolve = finish;
		reject = fail;
	});
	return { promise, resolve, reject };
}

test("lost create response reuses the same operation; reopening an existing terminal does not create", async () => {
	const user = userEvent.setup(),
		open = vi.fn();
	const existing = terminal();
	host.list.mockResolvedValue({
		serviceInstanceId: "service-a",
		terminals: [existing],
	});
	host.start
		.mockRejectedValueOnce(Error("disconnected"))
		.mockResolvedValueOnce(existing);
	render(choices("/root", null, open));
	await openChoices(user);
	await user.click(screen.getByRole("button", { name: "Terminal" }));
	await screen.findByText("Error: disconnected");
	await user.click(screen.getByRole("button", { name: "Terminal" }));
	await waitFor(() => expect(open).toHaveBeenCalledOnce());
	expect(host.start).toHaveBeenCalledTimes(2);
	expect(host.start.mock.calls[0][0]).toEqual(host.start.mock.calls[1][0]);
	expect(host.invokeHost).toHaveBeenCalledWith("terminal_manage", {
		request: { action: "start", request: host.start.mock.calls[0][0] },
	});
	await openChoices(user);
	await user.click(await screen.findByRole("button", { name: /pwsh 1/ }));
	expect(host.start).toHaveBeenCalledTimes(2);
	expect(open).toHaveBeenLastCalledWith(existing, "service-a", "pwsh 1");
});

test("workspace choices dismiss on Escape and restore the trigger focus", async () => {
	const user = userEvent.setup();
	render(choices("/root", null));
	const trigger = screen.getByRole("button", { name: "Add workspace tab" });
	await openChoices(user);
	await user.keyboard("{Escape}");
	await waitFor(() =>
		expect(screen.queryByRole("button", { name: "Files" })).toBeNull(),
	);
	expect(document.activeElement).toBe(trigger);
});

test("a lost create response from session A is not retried for session B in the same workspace", async () => {
	const user = userEvent.setup(),
		open = vi.fn();
	host.start
		.mockRejectedValueOnce(Error("disconnected"))
		.mockImplementation(async (request: StartRequest) =>
			terminal(request.sessionId, request.workspaceRoot),
		);
	const view = render(choices("/root", "session-a", open));
	await openChoices(user);
	await user.click(screen.getByRole("button", { name: "Terminal" }));
	await screen.findByText("Error: disconnected");
	view.rerender(choices("/root", "session-b", open));
	await user.click(screen.getByRole("button", { name: "Terminal" }));
	await waitFor(() => expect(open).toHaveBeenCalledOnce());
	const [first, second] = host.start.mock.calls.map(
		([request]) => request as StartRequest,
	);
	expect(first).toMatchObject({
		workspaceRoot: "/root",
		sessionId: "session-a",
		serviceInstanceId: "service-a",
	});
	expect(second).toMatchObject({
		workspaceRoot: "/root",
		sessionId: "session-b",
		serviceInstanceId: "service-a",
	});
	expect(second.operationId).not.toBe(first.operationId);
	expect(open).toHaveBeenCalledWith(
		terminal("session-b"),
		"service-a",
		"pwsh 1",
	);
});

test.each([
	["session", "/root", "session-b"],
	["workspace", "/other-root", "session-a"],
])(
	"a departed %s failure cannot publish its error or clear the current create's busy state",
	async (_boundary, root, sessionId) => {
		const user = userEvent.setup(),
			open = vi.fn();
		const first = pendingStart(),
			second = pendingStart();
		host.start
			.mockReturnValueOnce(first.promise)
			.mockReturnValueOnce(second.promise);
		const view = render(choices("/root", "session-a", open));
		await openChoices(user);
		await user.click(screen.getByRole("button", { name: "Terminal" }));
		view.rerender(choices(root, sessionId, open));
		await waitFor(() =>
			expect(
				(screen.getByRole("button", { name: "Terminal" }) as HTMLButtonElement)
					.disabled,
			).toBe(false),
		);
		await user.click(screen.getByRole("button", { name: "Terminal" }));
		await act(async () => first.reject(new Error("departed create failed")));
		expect(screen.queryByText("Error: departed create failed")).toBeNull();
		expect(
			(screen.getByRole("button", { name: "Starting…" }) as HTMLButtonElement)
				.disabled,
		).toBe(true);
		await act(async () => second.resolve(terminal(sessionId, root)));
		await waitFor(() =>
			expect(open).toHaveBeenCalledExactlyOnceWith(
				terminal(sessionId, root),
				"service-a",
				"pwsh 1",
			),
		);
		expect(host.start.mock.calls[1][0]).toMatchObject({
			workspaceRoot: root,
			sessionId,
		});
	},
);

test("a new service instance starts its own operation and rejects late errors from the replaced instance", async () => {
	const user = userEvent.setup(),
		open = vi.fn();
	const first = pendingStart(),
		second = pendingStart();
	host.start
		.mockReturnValueOnce(first.promise)
		.mockReturnValueOnce(second.promise);
	render(choices("/root", "session-a", open));
	await openChoices(user);
	await user.click(screen.getByRole("button", { name: "Terminal" }));
	await user.keyboard("{Escape}");
	await waitFor(() =>
		expect(screen.queryByRole("button", { name: "Files" })).toBeNull(),
	);
	host.list.mockResolvedValue({
		serviceInstanceId: "service-b",
		terminals: [],
	});
	await openChoices(user);
	await user.click(screen.getByRole("button", { name: "Terminal" }));
	await act(async () => first.reject(new Error("old service failed")));
	expect(screen.queryByText("Error: old service failed")).toBeNull();
	expect(
		(screen.getByRole("button", { name: "Starting…" }) as HTMLButtonElement)
			.disabled,
	).toBe(true);
	await act(async () => second.resolve(terminal("session-a")));
	await waitFor(() =>
		expect(open).toHaveBeenCalledExactlyOnceWith(
			terminal("session-a"),
			"service-b",
			"pwsh 1",
		),
	);
	const [older, current] = host.start.mock.calls.map(
		([request]) => request as StartRequest,
	);
	expect(current).toMatchObject({
		workspaceRoot: "/root",
		sessionId: "session-a",
		serviceInstanceId: "service-b",
	});
	expect(current.operationId).not.toBe(older.operationId);
});

test.each(["resolve", "reject"] as const)(
	"departed A/B completions cannot change a new A create after returning (%s old A)",
	async (outcome) => {
		const user = userEvent.setup(),
			open = vi.fn();
		const firstA = pendingStart(),
			firstB = pendingStart(),
			currentA = pendingStart();
		host.start
			.mockReturnValueOnce(firstA.promise)
			.mockReturnValueOnce(firstB.promise)
			.mockReturnValueOnce(currentA.promise)
			.mockResolvedValueOnce(terminal("session-a"));
		const view = render(choices("/root", "session-a", open));
		await openChoices(user);
		await user.click(screen.getByRole("button", { name: "Terminal" }));
		view.rerender(choices("/root", "session-b", open));
		await user.click(screen.getByRole("button", { name: "Terminal" }));
		view.rerender(choices("/root", "session-a", open));
		await user.click(screen.getByRole("button", { name: "Terminal" }));
		await act(async () => {
			if (outcome === "resolve") {
				firstA.resolve(terminal("session-a"));
				firstB.reject(new Error("departed B failed"));
			} else {
				firstA.reject(new Error("departed A failed"));
				firstB.resolve(terminal("session-b"));
			}
		});
		expect(open).not.toHaveBeenCalled();
		expect(screen.queryByRole("status")).toBeNull();
		expect(
			(screen.getByRole("button", { name: "Starting…" }) as HTMLButtonElement)
				.disabled,
		).toBe(true);
		await act(async () => currentA.reject(new Error("current A disconnected")));
		await screen.findByText("Error: current A disconnected");
		await user.click(screen.getByRole("button", { name: "Terminal" }));
		await waitFor(() =>
			expect(open).toHaveBeenCalledExactlyOnceWith(
				terminal("session-a"),
				"service-a",
				"pwsh 1",
			),
		);
		const requests = host.start.mock.calls.map(
			([request]) => request as StartRequest,
		);
		expect(requests[2].operationId).not.toBe(requests[0].operationId);
		expect(requests[3]).toEqual(requests[2]);
	},
);

test("terminal list failure has visible retry and cannot start without an authoritative service", async () => {
	const user = userEvent.setup();
	host.list
		.mockRejectedValueOnce(Error("Terminal service unavailable"))
		.mockResolvedValueOnce({ serviceInstanceId: "service-a", terminals: [] });
	render(choices("/root", null));
	await user.click(screen.getByRole("button", { name: "Add workspace tab" }));
	expect((await screen.findByRole("alert")).textContent).toContain(
		"Terminal service unavailable",
	);
	expect(
		(screen.getByRole("button", { name: "Terminal" }) as HTMLButtonElement)
			.disabled,
	).toBe(true);
	expect(host.start).not.toHaveBeenCalled();
	await user.click(screen.getByRole("button", { name: "Retry" }));
	await waitFor(() =>
		expect(
			(screen.getByRole("button", { name: "Terminal" }) as HTMLButtonElement)
				.disabled,
		).toBe(false),
	);
	expect(host.list).toHaveBeenCalledTimes(2);
});
