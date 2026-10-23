// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { Sidebar } from "../src/components/Sidebar";
import { sidebarProps } from "./helpers/sidebarFixture";
afterEach(cleanup);
async function editing(onRenameSession = vi.fn(async () => {})) {
	const user = userEvent.setup();
	render(<Sidebar {...sidebarProps({ onRenameSession })} />);
	const trigger = screen.getByRole("button", { name: "Chat actions Original" });
	await user.click(trigger);
	await screen.findByRole("menu");
	await user.click(screen.getByRole("menuitem", { name: "Rename Original" }));
	const input = await screen.findByRole("textbox", {
		name: "Conversation name",
	});
	await waitFor(() => expect(document.activeElement).toBe(input));
	return { user, input, trigger, onRenameSession };
}
test("rename has explicit Save and Cancel; blur retains the draft and Save trims it", async () => {
	const f = await editing();
	await f.user.clear(f.input);
	await f.user.type(f.input, "  Renamed  ");
	await f.user.tab();
	expect(f.onRenameSession).not.toHaveBeenCalled();
	expect((f.input as HTMLInputElement).value).toBe("  Renamed  ");
	await f.user.click(screen.getByRole("button", { name: "Save" }));
	await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
	expect(f.onRenameSession).toHaveBeenCalledExactlyOnceWith("s", "Renamed");
	await waitFor(() => expect(document.activeElement).toBe(f.trigger));
});
test("pending save cannot submit twice or dismiss on Escape", async () => {
	let finish!: () => void;
	const f = await editing(
		vi.fn(
			() =>
				new Promise<void>((resolve) => {
					finish = resolve;
				}),
		),
	);
	await f.user.clear(f.input);
	await f.user.type(f.input, "Renamed{Enter}{Enter}{Escape}");
	expect(f.onRenameSession).toHaveBeenCalledTimes(1);
	expect(screen.getByRole("dialog")).toBeTruthy();
	expect(
		(screen.getByRole("button", { name: "Saving…" }) as HTMLButtonElement)
			.disabled,
	).toBe(true);
	await act(async () => finish());
	await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
});
test("Escape cancels without saving, blank titles remain editable, and failed save can retry", async () => {
	const f = await editing(
		vi
			.fn()
			.mockRejectedValueOnce(Error("Save unavailable"))
			.mockResolvedValueOnce(undefined),
	);
	await f.user.clear(f.input);
	await f.user.type(f.input, "  ");
	expect(
		(screen.getByRole("button", { name: "Save" }) as HTMLButtonElement)
			.disabled,
	).toBe(true);
	await f.user.keyboard("{Escape}");
	await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
	expect(f.onRenameSession).not.toHaveBeenCalled();
	await f.user.click(f.trigger);
	await screen.findByRole("menu");
	await f.user.click(screen.getByRole("menuitem", { name: "Rename Original" }));
	const input = await screen.findByRole("textbox");
	await f.user.clear(input);
	await f.user.type(input, "Retry name");
	await f.user.click(screen.getByRole("button", { name: "Save" }));
	expect((await screen.findByRole("alert")).textContent).toContain(
		"Save unavailable",
	);
	expect((input as HTMLInputElement).value).toBe("Retry name");
	await f.user.click(screen.getByRole("button", { name: "Save" }));
	await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
	expect(f.onRenameSession).toHaveBeenCalledTimes(2);
});
test("application navigation is available when expanded and fully hidden when collapsed", () => {
  const props = sidebarProps();
  const view = render(<Sidebar {...props} />);
  expect(screen.getByRole("navigation", { name: "Application" })).toBeTruthy();
  expect(screen.getByRole("region", { name: "Pinned" })).toBeTruthy();
  expect(screen.getByRole("region", { name: "Recents" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Scheduled" })).toBeTruthy();
  view.rerender(<Sidebar {...props} collapsed />);
  expect(screen.queryByRole("complementary", { name: "Main navigation" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Settings" })).toBeNull();
  view.rerender(<Sidebar {...props} />);
  expect(screen.getByRole("button", { name: "Scheduled" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Settings" })).toBeTruthy();
});
