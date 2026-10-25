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
async function deleting(onDeleteSession = vi.fn(async () => {})) {
	const user = userEvent.setup();
	render(
		<Sidebar
			{...sidebarProps({ runningSessionIds: new Set(["s"]), onDeleteSession })}
		/>,
	);
	const trigger = screen.getByRole("button", { name: "Chat actions Original" });
	await user.click(trigger);
	await screen.findByRole("menu");
	await user.click(screen.getByRole("menuitem", { name: "Delete Original" }));
	const dialog = await screen.findByRole("dialog", {
		name: "Delete conversation?",
	});
	return { user, trigger, dialog, onDeleteSession };
}
test("running conversation deletion needs confirmation and pending delete cannot reenter", async () => {
	let finish!: () => void;
	const f = await deleting(
		vi.fn(
			() =>
				new Promise<void>((resolve) => {
					finish = resolve;
				}),
		),
	);
	expect(f.onDeleteSession).not.toHaveBeenCalled();
	expect(f.dialog.textContent).toContain(
		"Running work in this conversation will be stopped",
	);
	await f.user.click(within(f.dialog).getByRole("button", { name: "Delete" }));
	await f.user.keyboard("{Enter}{Escape}");
	expect(f.onDeleteSession).toHaveBeenCalledExactlyOnceWith("s");
	expect(screen.getByRole("dialog")).toBeTruthy();
	await act(async () => finish());
	await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
});
test("Escape or Cancel restores the conversation action trigger without deleting", async () => {
	const f = await deleting();
	await f.user.keyboard("{Tab}");
	expect(f.dialog.contains(document.activeElement)).toBe(true);
	await f.user.keyboard("{Escape}");
	await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
	await waitFor(() => expect(document.activeElement).toBe(f.trigger));
	await f.user.click(f.trigger);
	await screen.findByRole("menu");
	await f.user.click(screen.getByRole("menuitem", { name: "Delete Original" }));
	await f.user.click(await screen.findByRole("button", { name: "Cancel" }));
	await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
	expect(f.onDeleteSession).not.toHaveBeenCalled();
});
test("failed deletion retains the confirmation and offers retry", async () => {
	const f = await deleting(
		vi
			.fn()
			.mockRejectedValueOnce(Error("Delete unavailable"))
			.mockResolvedValueOnce(undefined),
	);
	await f.user.click(within(f.dialog).getByRole("button", { name: "Delete" }));
	expect((await screen.findByRole("alert")).textContent).toContain(
		"Delete unavailable",
	);
	await f.user.click(within(f.dialog).getByRole("button", { name: "Delete" }));
	await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
	expect(f.onDeleteSession).toHaveBeenCalledTimes(2);
});
