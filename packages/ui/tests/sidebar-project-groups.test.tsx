// @vitest-environment jsdom
import {
	act,
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
		within(screen.getByRole("region", { name: "Workspace D:/Empty" })).getByRole("button", { name: "Toggle Empty" }).getAttribute("aria-expanded"),
	).toBe("true");
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
test("workspace rename and removal stay independent of disclosure", async () => {
	const user = userEvent.setup(),
		rename = vi.fn(async () => true),
		remove = vi.fn(async () => true);
	render(
		<Sidebar
			{...props}
			onRenameWorkspace={rename}
			onRemoveWorkspace={remove}
		/>,
	);
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
	await user.click(screen.getByRole("menuitem", { name: "Remove" }));
	expect(remove).toHaveBeenCalledExactlyOnceWith("D:/A");
});

test("the entire workspace title toggles its conversation group without selecting a workspace", async () => {
  const user = userEvent.setup();
  const select = vi.fn(async () => true);
  const expand = vi.fn();
  render(<Sidebar {...props} activeWorkspaceRoot="D:/A" onSelectWorkspace={select} onExpandWorkspace={expand} />);
  const title = within(group()).getByText("A", { exact: true });
  const trigger = within(group()).getByRole("button", { name: "Toggle A" });
  await user.click(title);
  expect(within(group()).queryByRole("button", { name: /^new/ })).toBeNull();
  expect(trigger.getAttribute("aria-expanded")).toBe("false");
  expect(trigger.getAttribute("aria-current")).toBe("location");
  expect(select).not.toHaveBeenCalled();
  await user.keyboard(" ");
  expect(within(group()).getByRole("button", { name: /^new/ })).toBeTruthy();
  expect(expand).toHaveBeenCalledExactlyOnceWith("D:/A");
  expect(select).not.toHaveBeenCalled();
  const menu = within(group()).getByRole("button", { name: "Workspace actions A" });
  await user.click(menu);
  await screen.findByRole("menu");
  expect(trigger.getAttribute("aria-expanded")).toBe("true");
  await user.keyboard("{Escape}");
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  expect(document.activeElement).toBe(menu);
});

test("empty workspace titles still expand and collapse without a placeholder row", async () => {
  const user = userEvent.setup();
  render(<Sidebar {...props} />);
  const empty = within(screen.getByRole("region", { name: "Workspace D:/Empty" }));
  const title = empty.getByText("Empty", { exact: true });
  const trigger = empty.getByRole("button", { name: "Toggle Empty" });
  await user.click(title);
  expect(trigger.getAttribute("aria-expanded")).toBe("false");
  await user.click(title);
  expect(trigger.getAttribute("aria-expanded")).toBe("true");
  expect(empty.getAllByRole("button")).toHaveLength(2);
});

test("a failed rename retains the draft, supports retry, and restores menu focus", async () => {
  const user = userEvent.setup();
  const rename = vi.fn().mockRejectedValueOnce(new Error("Rename unavailable")).mockResolvedValueOnce(true);
  render(<Sidebar {...props} onRenameWorkspace={rename} />);
  const region = group();
  const trigger = within(region).getByRole("button", { name: "Workspace actions A" });
  await user.click(trigger);
  await user.click(await screen.findByRole("menuitem", { name: "Rename" }));
  const dialog = await screen.findByRole("dialog", { name: "Rename workspace" });
  const input = within(dialog).getByRole("textbox", { name: "Workspace name" });
  await waitFor(() => expect(document.activeElement).toBe(input));
  await user.clear(input);
  await user.type(input, "Retry name");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect((await within(dialog).findByRole("alert")).textContent).toBe("Rename unavailable");
  expect(input).toHaveProperty("value", "Retry name");
  expect(within(region).getByText("A", { exact: true })).toBeTruthy();
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(rename.mock.calls).toEqual([["D:/A", "Retry name"], ["D:/A", "Retry name"]]);
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  await waitFor(() => expect(document.activeElement).toBe(trigger));
});

test("pending removal leaves the title stable and usable, and a failed result can be retried", async () => {
  let finish!: (result: boolean) => void;
  const remove = vi.fn().mockImplementationOnce(() => new Promise(resolve => { finish = resolve; })).mockResolvedValueOnce(true);
  const select = vi.fn(async () => true);
  const user = userEvent.setup();
  render(<Sidebar {...props} activeWorkspaceRoot="D:/A" onRemoveWorkspace={remove} onSelectWorkspace={select} />);
  const title = within(group()).getByText("A", { exact: true });
  const disclosure = within(group()).getByRole("button", { name: "Toggle A" });
  const menu = within(group()).getByRole("button", { name: "Workspace actions A" });
  await user.click(menu);
  await user.click(await screen.findByRole("menuitem", { name: "Remove" }));
  expect(remove).toHaveBeenCalledExactlyOnceWith("D:/A");
  await user.click(title);
  expect(disclosure.getAttribute("aria-expanded")).toBe("false");
  expect(title.textContent).toBe("A");
  expect(select).not.toHaveBeenCalled();
  await act(async () => finish(false));
  expect((await within(group()).findByRole("alert")).textContent).toBe("The workspace action could not be completed.");
  await user.click(menu);
  await user.click(await screen.findByRole("menuitem", { name: "Remove" }));
  expect(remove.mock.calls).toEqual([["D:/A"], ["D:/A"]]);
  await waitFor(() => expect(within(group()).queryByRole("alert")).toBeNull());
});


test("closing navigation hides it completely and reopening retains workspace disclosure", async () => {
  const user = userEvent.setup();
  const view = render(<Sidebar {...props} />);
  await user.click(within(group()).getByRole("button", { name: "Toggle A" }));
  view.rerender(<Sidebar {...props} collapsed />);
  expect(screen.queryByRole("complementary", { name: "Main navigation" })).toBeNull();
  expect(screen.queryByRole("button", { name: "New chat" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Settings" })).toBeNull();
  view.rerender(<Sidebar {...props} />);
  expect(within(group()).getByRole("button", { name: "Toggle A" }).getAttribute("aria-expanded")).toBe("false");
});

test("navigation hover does not show a label and an empty catalog offers no folder picker", async () => {
  const user = userEvent.setup();
  render(<Sidebar {...props} workspaces={[]} sessions={[]} />);
  const newChat = screen.getByRole("button", { name: "New chat" });
  await user.hover(newChat);
  await act(async () => { await new Promise(resolve => setTimeout(resolve, 60)); });
  expect(screen.queryByRole("tooltip")).toBeNull();
  expect(screen.queryByRole("button", { name: /Open folder/i })).toBeNull();
  await user.click(newChat);
  expect(props.onNewChat).toHaveBeenCalledOnce();
});


test.each(["Workspace actions A", "Chat actions pin-a"])("closing navigation dismisses %s without reopening it later", async (label) => {
  const user = userEvent.setup();
  const view = render(<Sidebar {...props} />);
  await user.click(screen.getByRole("button", { name: label }));
  expect(await screen.findByRole("menu")).toBeTruthy();
  view.rerender(<Sidebar {...props} collapsed />);
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  view.rerender(<Sidebar {...props} />);
  expect(screen.queryByRole("menu")).toBeNull();
  expect(within(group()).getByRole("button", { name: "Toggle A" }).getAttribute("aria-expanded")).toBe("true");
});

test("running alone has no attention dot; unread output and reply requests do", () => {
  const base = sidebarProps({ currentSessionId: null, runningSessionIds: new Set(["s"]) });
  const view = render(<Sidebar {...base} />);
  expect(screen.queryByRole("status", { name: "Unread messages" })).toBeNull();
  expect(screen.getByRole("status", { name: "Running" })).toBeTruthy();
  view.rerender(<Sidebar {...base} completedSessionIds={new Set(["s"])} />);
  expect(screen.getByRole("status", { name: "Unread messages" })).toBeTruthy();
  view.rerender(<Sidebar {...base} currentSessionId="s" completedSessionIds={new Set(["s"])} />);
  expect(screen.queryByRole("status", { name: "Unread messages" })).toBeNull();
  expect(screen.getByRole("status", { name: "Running" })).toBeTruthy();
  view.rerender(<Sidebar {...base} currentSessionId="s" waitingSessionIds={new Set(["s"])} />);
  expect(screen.getByRole("status", { name: "Reply needed" })).toBeTruthy();
  view.rerender(<Sidebar {...base} currentSessionId="s" waitingSessionIds={new Set()} />);
  expect(screen.queryByRole("status", { name: "Unread messages" })).toBeNull();
  expect(screen.getByRole("status", { name: "Running" })).toBeTruthy();
});


test("compact chat metadata shows age at rest and an accessible indicator during activity", () => {
  const title = "先看下这个项目";
  const base = { ...props, sessions: [{ ...session("compact", "D:/A", Date.now() - 54 * 60_000), title }] };
  const view = render(<Sidebar {...base} />);
  const row = () => within(screen.getByRole("button", { name: new RegExp("^" + title) }));
  expect(row().getByText("54m ago")).toBeTruthy();
  view.rerender(<Sidebar {...base} runningSessionIds={new Set(["compact"])} />);
  expect(row().getByRole("status", { name: "Running" })).toBeTruthy();
  expect(row().queryByText("Running")).toBeNull();
  expect(row().queryByText("54m ago")).toBeNull();
  view.rerender(<Sidebar {...base} waitingSessionIds={new Set(["compact"])} />);
  expect(row().getByRole("status", { name: "Reply needed" })).toBeTruthy();
  expect(row().queryByText("Needs reply")).toBeNull();
  view.rerender(<Sidebar {...base} completedSessionIds={new Set(["compact"])} />);
  expect(row().getByRole("status", { name: "Unread messages" })).toBeTruthy();
  expect(row().queryByText("Unread")).toBeNull();
  view.rerender(<Sidebar {...base} />);
  expect(row().getByText("54m ago")).toBeTruthy();
});
