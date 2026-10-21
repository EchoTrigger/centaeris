// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import {
	SummaryPanel,
	type SummaryPanelTab,
} from "../src/components/SummaryPanel";
import { useState } from "react";
afterEach(cleanup);
vi.mock("../src/components/CodePreview", () => ({ default: () => <pre /> }));
vi.mock("../src/components/WorkspaceFilesPanel", () => ({
	WorkspaceFilesPanel: ({
		onOpenFile,
	}: {
		onOpenFile: (entry: { path: string }) => void;
	}) => {
		const [expanded, setExpanded] = useState(false);
		return (
			<div data-testid="file-tree">
				<button onClick={() => setExpanded(true)}>Expand</button>
				{expanded ? (
					<button onClick={() => onOpenFile({ path: "a.md" })}>
						Open nested file
					</button>
				) : null}
			</div>
		);
	},
}));
vi.mock("../src/components/chat/AgentSessionPreview", () => ({
	AgentSessionPreview: ({ sessionId }: { sessionId: string }) => {
		const [page, setPage] = useState(1);
		return (
			<button
				data-testid={`agent-${sessionId}`}
				onClick={() => setPage(page + 1)}
			>
				{page}
			</button>
		);
	},
}));

vi.mock("../src/components/WorkspaceTasks", () => ({
	WorkspaceTasks: ({ active }: { active: boolean }) => (
		<div data-testid="task-observer" data-active={active} />
	),
}));
const entries: SummaryPanelTab[] = [
	{ id: "a", title: "a.md", kind: "file", path: "a.md", content: "A" },
	{ id: "b", title: "b.md", kind: "file", path: "b.md", content: "B" },
];
test("library tab keyboard navigation and independent close controls select and close the intended content", async () => {
	const user = userEvent.setup(),
		select = vi.fn(),
		close = vi.fn();
	function Fixture() {
		const [tabs, setTabs] = useState(entries),
			[active, setActive] = useState("a");
		return (
			<SummaryPanel
				tabs={tabs}
				activeTabId={active}
				onSelectTab={(id) => {
					select(id);
					setActive(id);
				}}
				onCloseTab={(id) => {
					close(id);
					setTabs(tabs.filter((tab) => tab.id !== id));
					setActive(id === "a" ? "b" : "a");
				}}
			/>
		);
	}
	render(<Fixture />);
	const a = screen.getByRole("tab", { name: "a.md" });
	a.focus();
	await user.keyboard("{ArrowRight}");
	expect(select).toHaveBeenCalledWith("b");
	expect(document.activeElement).toBe(
		screen.getByRole("tab", { name: "b.md" }),
	);
	expect(
		screen.getByRole("tab", { name: "b.md" }).getAttribute("aria-selected"),
	).toBe("true");
	await user.keyboard("{Home}");
	expect(document.activeElement).toBe(a);
	await user.click(screen.getByRole("button", { name: "Close a.md" }));
	expect(close).toHaveBeenCalledExactlyOnceWith("a");
	expect(screen.queryByRole("tab", { name: "a.md" })).toBeNull();
	await waitFor(() =>
		expect(document.activeElement).toBe(
			screen.getByRole("tab", { name: "b.md" }),
		),
	);
	await user.keyboard("{Delete}");
	expect(close).toHaveBeenLastCalledWith("b");
});
test("file browser expansion survives replacing the Files landing tab with a file", async () => {
	const user = userEvent.setup(),
		open = vi.fn();
	const element = (tabs: SummaryPanelTab[]) => (
		<SummaryPanel
			tabs={tabs}
			activeTabId={tabs[0].id}
			onSelectTab={vi.fn()}
			onCloseTab={vi.fn()}
			onOpenWorkspacePath={open}
		/>
	);
	const view = render(
		element([
			{ id: "files", kind: "files", title: "Files", workspaceRoot: "root" },
		]),
	);
	const tree = screen.getByTestId("file-tree");
	await user.click(screen.getByRole("button", { name: "Expand" }));
	view.rerender(element([{ ...entries[0], workspaceRoot: "root" }]));
	expect(screen.getByTestId("file-tree")).toBe(tree);
	await user.click(screen.getByRole("button", { name: "Open nested file" }));
	expect(open).toHaveBeenCalledExactlyOnceWith("a.md");
});
test("switching keeps each agent view state and closing releases only that view", async () => {
	const user = userEvent.setup();
	const tabs: SummaryPanelTab[] = [
		{ id: "a", title: "Agent A", kind: "agent", sessionId: "a" },
		{ id: "b", title: "Agent B", kind: "agent", sessionId: "b" },
	];
	const element = (activeTabId: string, entries = tabs) => (
		<SummaryPanel
			tabs={entries}
			activeTabId={activeTabId}
			onSelectTab={vi.fn()}
			onCloseTab={vi.fn()}
		/>
	);
	const view = render(element("a"));
	const agent = screen.getByTestId("agent-a");
	await user.click(agent);
	view.rerender(element("b"));
	expect(screen.getByTestId("agent-a")).toBe(agent);
	expect(agent.textContent).toBe("2");
	expect(screen.getByRole("tabpanel").id).toBe("panel-b");
	view.rerender(element("a"));
	expect(screen.getByTestId("agent-a").textContent).toBe("2");
	view.rerender(element("a", [tabs[0]]));
	expect(screen.queryByTestId("agent-b")).toBeNull();
	expect(screen.getByTestId("agent-a")).toBe(agent);
});
test("task observation follows selected tab and preview visibility", () => {
	const element = (activeTabId: string, visible = true) => (
		<SummaryPanel
			visible={visible}
			tabs={[
				{ id: "task", title: "Task", kind: "tasks", sessionId: "s1" },
				entries[0],
			]}
			activeTabId={activeTabId}
			onSelectTab={vi.fn()}
			onCloseTab={vi.fn()}
		/>
	);
	const view = render(element("task"));
	expect(screen.getByTestId("task-observer").getAttribute("data-active")).toBe(
		"true",
	);
	view.rerender(element("a"));
	expect(screen.getByTestId("task-observer").getAttribute("data-active")).toBe(
		"false",
	);
	view.rerender(element("task", false));
	expect(screen.getByTestId("task-observer").getAttribute("data-active")).toBe(
		"false",
	);
});
test("all-tabs menu exposes long overflowing content, and file failures have a retry action", async () => {
	const user = userEvent.setup(),
		select = vi.fn(),
		refresh = vi.fn();
	render(
		<SummaryPanel
			tabs={[
				{ ...entries[0], error: "File unavailable" },
				{ ...entries[1], title: "非常长的工作区文档名称与更多内容.md" },
			]}
			activeTabId="a"
			onSelectTab={select}
			onCloseTab={vi.fn()}
			onRefreshFile={refresh}
		/>,
	);
	expect(screen.getByRole("alert").textContent).toContain("File unavailable");
	await user.click(screen.getByRole("button", { name: "Retry" }));
	expect(refresh).toHaveBeenCalledOnce();
	await user.click(screen.getByRole("button", { name: "All open tabs" }));
	await screen.findByRole("menu");
	expect(
		screen
			.getByRole("menuitemradio", { name: "a.md" })
			.getAttribute("aria-checked"),
	).toBe("true");
	await user.click(
		screen.getByRole("menuitemradio", {
			name: "非常长的工作区文档名称与更多内容.md",
		}),
	);
	expect(select).toHaveBeenCalledWith("b");
});
