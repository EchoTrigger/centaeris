// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { configureApi, clearCsrfToken } from "../../src/api";
import { i18n } from "../../src/i18n";
import { WorkspaceInbox } from "../../src/work/WorkspaceInbox";
const fresh = () => ({ id: "notice", workspaceId: "one", issueId: "issue", issueTitle: "Review evidence", kind: "assigned", event: { actor: { id: "actor", name: "Alex" }, changes: { assigneeId: { before: null, after: "user" } } }, createdAt: "2026-11-12T01:00:00Z", readAt: null as string | null, archivedAt: null as string | null });
let notification = fresh();
let fail = false;
let patches: unknown[] = [];
beforeEach(async () => {
  sessionStorage.clear(); notification = fresh(); fail = false; patches = []; clearCsrfToken(); configureApi({ apiBaseUrl: "http://localhost" }); await i18n.changeLanguage("en");
  vi.stubGlobal("fetch", vi.fn(async (input, options) => {
    const url = new URL(String(input));
    if (url.pathname === "/api/csrf") return Response.json({ csrfToken: "token" });
    if (options?.method === "PATCH") {
      const body = JSON.parse(options.body); patches.push(body);
      if (fail) return Response.json({ error: "receipt_rejected" }, { status: 500 });
      if (body.read !== undefined) notification.readAt = body.read ? "2026-11-13T01:00:00Z" : null;
      if (body.archived !== undefined) notification.archivedAt = body.archived ? "2026-11-13T01:00:00Z" : null;
      return Response.json({ notification });
    }
    if (url.pathname.endsWith("/notice")) return Response.json({ notification });
    const view = url.searchParams.get("view");
    const visible = view === "archived" ? notification.archivedAt : !notification.archivedAt && (view !== "unread" || !notification.readAt);
    return Response.json({ notifications: visible ? [notification] : [], total: visible ? 1 : 0, unreadCount: notification.readAt ? 0 : 1 });
  }));
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });
function mount(route = "/w/one/work/inbox") {
  const router = createMemoryRouter([{ path: "/w/one/work/inbox", element: <WorkspaceInbox userId="user" workspaceId="one" /> }], { initialEntries: [route] });
  render(<RouterProvider router={router} />); return router;
}
const list = () => within(screen.getByRole("region", { name: "Notifications" }));
test("server receipts drive read, unread, archive and restore while selected unread detail remains readable", async () => {
  const user = userEvent.setup(); const router = mount("/w/one/work/inbox?view=unread&offset=0");
  await user.click(await list().findByRole("button", { name: /Review evidence/ }));
  await screen.findByRole("button", { name: "Mark as unread" });
  expect(list().queryByRole("button", { name: /Review evidence/ })).toBeNull();
  expect(screen.getByRole("heading", { name: "Review evidence" })).toBeTruthy();
  const link = screen.getByRole("link", { name: "Open Issues" });
  expect(link.getAttribute("href")).toContain("/work/tasks/issue?returnTo=%2Fw%2Fone%2Fwork%2Finbox%3Fview%3Dunread%26offset%3D0%26notification%3Dnotice");
  await user.click(screen.getByRole("button", { name: "Mark as unread" }));
  await screen.findByRole("button", { name: "Mark as read" });
  await user.click(screen.getByRole("button", { name: "Archive notification" }));
  await screen.findByRole("button", { name: "Restore to inbox" });
  await user.click(screen.getByRole("button", { name: "Archived", exact: true }));
  await user.click(await list().findByRole("button", { name: /Review evidence/ }));
  await user.click(await screen.findByRole("button", { name: "Restore to inbox" }));
  await screen.findByText("No archived notifications");
  expect(patches).toContainEqual({ archived: true }); expect(patches).toContainEqual({ archived: false });
  expect(router.state.location.search).toContain("notification=notice");
});
test("failed acknowledgment preserves URL selection and unread receipt; remount and focus read server state", async () => {
  fail = true; const router = mount("/w/one/work/inbox?view=unread&notification=notice");
  await screen.findByRole("alert");
  expect(screen.getByRole("button", { name: "Mark as read" })).toBeTruthy();
  expect(router.state.location.search).toContain("notification=notice");
  const viewport = screen.getByRole("region", { name: "Notifications" });
  viewport.scrollTop = 137; fireEvent.scroll(viewport);
  cleanup(); fail = false; notification.readAt = "2026-11-13T01:00:00Z";
  mount("/w/one/work/inbox?view=unread&notification=notice");
  await screen.findByRole("button", { name: "Mark as unread" });
  expect(screen.getByRole("region", { name: "Notifications" }).scrollTop).toBe(137);
  await waitFor(() => expect(list().queryByRole("button", { name: /Review evidence/ })).toBeNull());
  notification.readAt = "2026-11-13T01:00:00Z"; notification.issueTitle = "Updated evidence";
  await act(async () => { window.dispatchEvent(new Event("focus")); });
  expect(await screen.findByRole("heading", { name: "Updated evidence" })).toBeTruthy();
});
