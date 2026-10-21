// @vitest-environment jsdom
import {
	cleanup,
	render,
	screen,
	waitFor,
	within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { Sidebar } from "../src/components/Sidebar";
import { sidebarProps } from "./helpers/sidebarFixture";
afterEach(cleanup);
const session = (
	id: string,
	cwd: string,
	updatedAt: number,
	isPinned = false,
) => ({
	id,
	title: id,
	cwd,
	updatedAt,
	isPinned,
	sessionKind: "main" as const,
	messageCount: 1,
});
const props = sidebarProps({
	sessions: [
		session("old", "D:/A", 1),
		session("b", "D:/B", 2),
		session("new", "D:/A", 3),
		session("pin-a", "D:/A", 4, true),
		session("pin-b", "D:/B", 5, true),
	],
	workspaces: ["A", "B", "Empty"].map((name) => ({
		root: `D:/${name}`,
		name,
		sortOrder: 0,
		updatedAt: 0,
	})),
	activeWorkspaceRoot: null,
});
const group = () => screen.getByRole("region", { name: "Workspace D:/A" });
test("conversations group by workspace, newest first; pinned stays global and empty workspace is visible", () => {
	render(<Sidebar {...props} />);
	expect(
		within(group())
			.getAllByRole("button", { name: /^(new|old)/ })
			.map((button) => button.textContent),
	).toEqual([expect.stringContaining("new"), expect.stringContaining("old")]);
	expect(
		screen.getByRole("region", { name: "Workspace D:/Empty" }).textContent,
	).toContain("No conversations yet");
	const pinned = within(screen.getByRole("region", { name: "Pinned" }));
	expect(
		pinned
			.getAllByRole("button", { name: /^pin-/ })
			.map((button) => button.textContent),
	).toEqual([
		expect.stringContaining("pin-b"),
		expect.stringContaining("pin-a"),
	]);
});
test("removal hides the workspace group without changing sessions and reopening restores it", () => {
	const view = render(
		<Sidebar
			{...props}
			workspaces={props.workspaces.filter(
				(workspace) => workspace.name !== "A",
			)}
		/>,
	);
	expect(screen.queryByRole("region", { name: "Workspace D:/A" })).toBeNull();
	expect(
		props.sessions.filter((session) => session.cwd === "D:/A"),
	).toHaveLength(3);
	view.rerender(<Sidebar {...props} />);
	expect(within(group()).getByRole("button", { name: /^new/ })).toBeTruthy();
});
test("workspace selection, disclosure and rename/remove are distinct actions", async () => {
	const user = userEvent.setup(),
		rename = vi.fn(async () => true),
		remove = vi.fn(async () => true),
		select = vi.fn(async () => true);
	render(
		<Sidebar
			{...props}
			onRenameWorkspace={rename}
			onRemoveWorkspace={remove}
			onSelectWorkspace={select}
		/>,
	);
	await user.click(within(group()).getByRole("button", { name: "Toggle A" }));
	expect(within(group()).queryByRole("button", { name: /^new/ })).toBeNull();
	expect(select).not.toHaveBeenCalled();
	await user.click(
		within(group()).getByRole("button", { name: "Select workspace A" }),
	);
	expect(select).toHaveBeenCalledExactlyOnceWith("D:/A");
	expect(within(group()).queryByRole("button", { name: /^new/ })).toBeNull();
	await user.click(within(group()).getByRole("button", { name: "Toggle A" }));
	expect(within(group()).getByRole("button", { name: /^new/ })).toBeTruthy();
	const trigger = within(group()).getByRole("button", {
		name: "Workspace actions A",
	});
	await user.click(trigger);
	await screen.findByRole("menu");
	await user.click(screen.getByRole("menuitem", { name: "Rename" }));
	const input = await screen.findByRole("textbox", { name: "Workspace name" });
	await user.clear(input);
	await user.type(input, "Renamed");
	await user.click(screen.getByRole("button", { name: "Save" }));
	expect(rename).toHaveBeenCalledExactlyOnceWith("D:/A", "Renamed");
	await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
	await user.click(trigger);
	await screen.findByRole("menu");
	await user.click(screen.getByRole("menuitem", { name: "Remove workspace" }));
	expect(remove).toHaveBeenCalledExactlyOnceWith("D:/A");
});
