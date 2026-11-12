// @vitest-environment jsdom
import { act, cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { afterEach, beforeEach, expect, test } from "vitest";
import { i18n } from "../../src/i18n";
import { DemoInbox } from "../../src/work/DemoInbox";

beforeEach(async () => { sessionStorage.clear(); await i18n.changeLanguage("en"); });
afterEach(cleanup);
function mount(workspaceId = "one", route = "/inbox") {
  const router = createMemoryRouter([{ path: "/inbox", element: <DemoInbox key={workspaceId} userId="user" workspaceId={workspaceId} /> }], { initialEntries: [route] });
  render(<RouterProvider router={router} />);
  return router;
}
test("Inbox acknowledges, archives and restores notifications without changing their linked issue", async () => {
  const user = userEvent.setup();
  const router = mount();
  const list = () => within(screen.getByRole("region", { name: "Notifications" }));
  await user.click(list().getByRole("button", { name: /Review toolkit release materials/ }));
  expect(router.state.location.search).toBe("?notification=review");
  expect(screen.getByRole("link", { name: "Open CW-12" }).getAttribute("href")).toBe("/w/one/work/tasks/CW-12");
  expect(screen.getByRole("button", { name: "Mark as unread" })).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Archive notification" }));
  expect(list().queryByRole("button", { name: /Review toolkit release materials/ })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Archived" }));
  await user.click(list().getByRole("button", { name: /Review toolkit release materials/ }));
  await user.click(screen.getByRole("button", { name: "Restore to inbox" }));
  expect(screen.getByText("No archived notifications")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Inbox" }));
  expect(list().getByRole("button", { name: /Review toolkit release materials/ })).toBeTruthy();
});
test("read receipts survive remount, workspace scopes stay separate, and URL selection follows history", async () => {
  const user = userEvent.setup();
  const router = mount();
  await user.click(screen.getByRole("button", { name: /Review toolkit release materials/ }));
  await user.click(screen.getByRole("button", { name: /Clarify retrieval access boundaries/ }));
  await act(async () => { await router.navigate(-1); });
  expect(screen.getByRole("heading", { name: "Review toolkit release materials" })).toBeTruthy();
  cleanup();
  mount("one", "/inbox?view=unread");
  expect(screen.queryByRole("button", { name: /Review toolkit release materials/ })).toBeNull();
  expect(screen.getByRole("button", { name: /Draft toolkit release notes/ })).toBeTruthy();
  cleanup();
  mount("two", "/inbox?view=unread");
  expect(screen.getByRole("button", { name: /Review toolkit release materials/ })).toBeTruthy();
});
