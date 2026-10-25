// @vitest-environment jsdom
import {
	act,
	cleanup,
	fireEvent,
	render,
	screen,
	waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, afterEach, expect, test, vi } from "vitest";
import { WorkspaceOverview } from "../src/components/WorkspaceOverview";
import { listProcesses } from "../src/lib/processBridge";
vi.mock("../src/lib/processBridge", () => ({ listProcesses: vi.fn() }));
vi.mock("../src/lib/workspaceBridge", () => ({
	getWorkspaceGitStatus: vi.fn(async () => ({ isGitRepository: false })),
}));
const props = {
	name: "Workspace",
	root: "root",
	agents: [],
	onFiles: vi.fn(),
	onReview: vi.fn(),
	onAgent: vi.fn(),
	onTasks: vi.fn(),
};
beforeEach(() => vi.clearAllMocks());
afterEach(() => {
	cleanup();
	vi.useRealTimers();
});
test("Task appears only after process discovery and opens its authoritative view", async () => {
	vi.mocked(listProcesses).mockResolvedValue({
		serviceInstanceId: "epoch",
		processes: [],
	});
	vi.useFakeTimers();
	render(<WorkspaceOverview {...props} sessionId="s1" />);
	await act(async () => {
		fireEvent.click(screen.getByRole("button", { name: "Workspace overview" }));
		await vi.advanceTimersByTimeAsync(10);
	});
	expect(screen.queryByRole("button", { name: "Task" })).toBeNull();
	vi.mocked(listProcesses).mockResolvedValue({
		serviceInstanceId: "epoch",
		processes: [{ sessionId: "s1" } as never],
	});
	await act(async () => {
		await vi.advanceTimersByTimeAsync(1500);
	});
	expect(screen.getByRole("button", { name: "Task" }).textContent).toContain(
		"1",
	);
	fireEvent.click(screen.getByRole("button", { name: "Task" }));
	expect(props.onTasks).toHaveBeenCalledOnce();
});
test("old session's late process count cannot reveal Task for a new session", async () => {
	let resolve!: (value: Awaited<ReturnType<typeof listProcesses>>) => void;
	vi.mocked(listProcesses).mockImplementation((id) =>
		id === "s1"
			? new Promise((finish) => {
					resolve = finish;
				})
			: Promise.resolve({ serviceInstanceId: "epoch", processes: [] }),
	);
	const user = userEvent.setup(),
		view = render(<WorkspaceOverview {...props} sessionId="s1" />);
	await user.click(screen.getByRole("button", { name: "Workspace overview" }));
	await screen.findByRole("dialog", { name: "Workspace" });
	view.rerender(<WorkspaceOverview {...props} sessionId="s2" />);
	await act(async () =>
		resolve({ serviceInstanceId: "epoch", processes: [{} as never] }),
	);
	expect(screen.queryByRole("button", { name: "Task" })).toBeNull();
});
test("visible resource entry supports keyboard dismissal, focus restoration and workspace actions", async () => {
	const user = userEvent.setup();
	render(<WorkspaceOverview {...props} />);
	const trigger = screen.getByRole("button", { name: "Workspace overview" });
	expect(trigger.getAttribute("aria-label")).toBe("Workspace overview");
	await user.click(trigger);
	await screen.findByRole("dialog", { name: "Workspace" });
	await user.keyboard("{Escape}");
	await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
	expect(document.activeElement).toBe(trigger);
	await user.click(trigger);
	await user.click(await screen.findByRole("button", { name: "Browse files" }));
	expect(props.onFiles).toHaveBeenCalledOnce();
	await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
});
