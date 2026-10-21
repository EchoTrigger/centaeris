// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { WorkspaceSwitcher } from "../src/components/WorkspaceSwitcher";
const workspaces = [
	{
		root: "D:/项目/长期工作区",
		name: "长期工作区",
		sortOrder: 0,
		updatedAt: 1,
	},
	{ root: "D:/other", name: "Other", sortOrder: 1, updatedAt: 2 },
];
afterEach(cleanup);
test("current workspace and complete path stay discoverable and the selector marks the current item", async () => {
	const select = vi.fn(async () => {}),
		user = userEvent.setup();
	render(
		<WorkspaceSwitcher
			workspaces={workspaces}
			activeRoot={workspaces[0].root}
			onSelect={select}
			onOpen={vi.fn()}
		/>,
	);
	const trigger = screen.getByRole("button", { name: /Switch workspace/ });
	expect(trigger.textContent).toContain("长期工作区");
	expect(trigger.getAttribute("title")).toBe(workspaces[0].root);
	await user.click(trigger);
	await screen.findByRole("menu");
	expect(
		screen
			.getByRole("menuitemradio", { name: /长期工作区/ })
			.getAttribute("aria-checked"),
	).toBe("true");
	await user.click(screen.getByRole("menuitemradio", { name: /Other/ }));
	await waitFor(() =>
		expect(select).toHaveBeenCalledExactlyOnceWith("D:/other"),
	);
});
test("switching is guarded while pending and failures leave a visible retry path", async () => {
	let reject!: (error: Error) => void;
	const select = vi.fn(
		() =>
			new Promise<void>((_resolve, fail) => {
				reject = fail;
			}),
	);
	const user = userEvent.setup();
	render(
		<WorkspaceSwitcher
			workspaces={workspaces}
			activeRoot={workspaces[0].root}
			onSelect={select}
			onOpen={vi.fn()}
		/>,
	);
	await user.click(screen.getByRole("button", { name: /Switch workspace/ }));
	await screen.findByRole("menu");
	await user.click(screen.getByRole("menuitemradio", { name: /Other/ }));
	expect(
		(
			screen.getByRole("button", {
				name: /Switch workspace/,
			}) as HTMLButtonElement
		).disabled,
	).toBe(true);
	reject(new Error("Folder is unavailable"));
	expect(await screen.findByRole("alert")).toHaveProperty(
		"textContent",
		"Folder is unavailable",
	);
	await waitFor(() =>
		expect(
			(
				screen.getByRole("button", {
					name: /Switch workspace/,
				}) as HTMLButtonElement
			).disabled,
		).toBe(false),
	);
});
test("an empty or collapsed sidebar still exposes real open/create actions and Escape returns focus", async () => {
	const open = vi.fn(),
		user = userEvent.setup();
	const view = render(
		<WorkspaceSwitcher
			collapsed
			workspaces={[]}
			activeRoot={null}
			onSelect={vi.fn()}
			onOpen={open}
		/>,
	);
	const trigger = screen.getByRole("button", { name: /Switch workspace/ });
	await user.click(trigger);
	await screen.findByRole("menu");
	expect(screen.getByText("No recent workspaces")).toBeDefined();
	await user.keyboard("{Escape}");
	await waitFor(() => expect(document.activeElement).toBe(trigger));
	await user.click(trigger);
	await screen.findByRole("menu");
	await user.click(screen.getByRole("menuitem", { name: "Open folder…" }));
	expect(open).toHaveBeenCalledExactlyOnceWith("customPath");
	view.rerender(
		<WorkspaceSwitcher
			workspaces={[]}
			activeRoot={null}
			onSelect={vi.fn()}
			onOpen={open}
		/>,
	);
	await user.click(screen.getByRole("button", { name: /Switch workspace/ }));
	await screen.findByRole("menu");
	await user.click(screen.getByRole("menuitem", { name: "New workspace" }));
	expect(open).toHaveBeenLastCalledWith("defaultDirectory");
});
