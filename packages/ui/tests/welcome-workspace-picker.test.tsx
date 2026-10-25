// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { WelcomeWorkspacePicker } from "../src/components/chat/WelcomeWorkspacePicker";
afterEach(cleanup);
const workspaces = [{ root: "D:/A", name: "A", sortOrder: 0, updatedAt: 1 }, { root: "D:/B", name: "B", sortOrder: 1, updatedAt: 1 }];

test("empty workspace menu opens a folder only after an explicit choice", async () => {
  const user = userEvent.setup(), open = vi.fn(async () => {}), select = vi.fn(async () => {}), focus = vi.fn();
  render(<WelcomeWorkspacePicker workspaces={[]} onOpenWorkspace={open} onSelectWorkspace={select} onFocusComposer={focus} />);
  expect(open).not.toHaveBeenCalled();
  await user.click(screen.getByRole("button", { name: "Choose workspace" }));
  const items = await screen.findAllByRole("menuitem");
  expect(items).toHaveLength(1);
  expect(items[0].textContent).toContain("Open folder…");
  await user.click(items[0]);
  await waitFor(() => expect(focus).toHaveBeenCalledOnce());
  expect(open).toHaveBeenCalledOnce(); expect(select).not.toHaveBeenCalled();
  expect(screen.queryByRole("menu")).toBeNull();
});

test("recent workspaces preserve host order and select through the existing action", async () => {
  const user = userEvent.setup(), select = vi.fn(async () => {}), open = vi.fn(async () => {});
  render(<WelcomeWorkspacePicker workspaces={workspaces} onOpenWorkspace={open} onSelectWorkspace={select} />);
  await user.click(screen.getByRole("button", { name: "Choose workspace" }));
  const items = await screen.findAllByRole("menuitemradio");
  expect(items.map(item => item.textContent)).toEqual(["AD:/A", "BD:/B"]);
  await user.click(items[1]);
  await waitFor(() => expect(select).toHaveBeenCalledExactlyOnceWith("D:/B"));
  expect(open).not.toHaveBeenCalled();
});

test("a failed folder choice keeps a readable error and permits retry", async () => {
  const user = userEvent.setup(), open = vi.fn().mockRejectedValueOnce(new Error("Folder unavailable")).mockResolvedValueOnce(undefined);
  render(<WelcomeWorkspacePicker workspaces={[]} onOpenWorkspace={open} onSelectWorkspace={vi.fn(async () => {})} />);
  await user.click(screen.getByRole("button", { name: "Choose workspace" }));
  await user.click(await screen.findByRole("menuitem", { name: /Open folder/ }));
  expect((await screen.findByRole("alert")).textContent).toBe("Folder unavailable");
  await user.click(screen.getByRole("button", { name: "Choose workspace" }));
  await user.click(await screen.findByRole("menuitem", { name: /Open folder/ }));
  await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
  expect(open).toHaveBeenCalledTimes(2);
});

test("a selected workspace stays selectable from the named trigger", async () => {
  const user = userEvent.setup(), select = vi.fn(async () => {}), open = vi.fn(async () => {});
  render(<WelcomeWorkspacePicker workspace={workspaces[0]} workspaces={workspaces} onOpenWorkspace={open} onSelectWorkspace={select} />);
  const trigger = screen.getByRole("button", { name: "Choose workspace: A" });
  expect(trigger.textContent).toBe("A");
  await user.click(trigger);
  expect((await screen.findByRole("menuitemradio", { checked: true })).textContent).toBe("AD:/A");
  await user.click(screen.getAllByRole("menuitemradio")[1]);
  await waitFor(() => expect(select).toHaveBeenCalledExactlyOnceWith("D:/B"));
  expect(open).not.toHaveBeenCalled();
});
