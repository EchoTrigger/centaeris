// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { createMemoryRouter, RouterProvider } from "react-router";
import { WorkspaceSearch } from "../../src/shell/WorkspaceSearch";

const request = vi.hoisted(() => vi.fn());
vi.mock("../../src/api", () => ({ apiJson: (...args: unknown[]) => request(...args) }));
vi.mock("../../src/i18n", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));

const hit = (id: string, kind = "final", snippet = "权限核对完成") => ({
  sessionId: id, title: `Conversation ${id}`, updatedAt: "2026-11-11T00:00:00Z",
  match: { kind, snippet, eventId: `event-${id}`, messageId: `message-${id}`, sourceSequence: "3" },
});

beforeEach(() => {
  request.mockReset();
  request.mockImplementation(async (path: string) => path === "/api/library"
    ? { objects: [] } : { matches: [], nextCursor: null });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

function openSearch() {
  const router = createMemoryRouter([
    { path: "/", element: <WorkspaceSearch workspaceId="research" onClose={() => {}} /> },
    { path: "/w/research/sessions/:id", element: <div>Selected conversation</div> },
  ]);
  render(<RouterProvider router={router} />);
  return router;
}

test("body hits show their type and snippet and open the selected conversation by keyboard", async () => {
  const user = userEvent.setup();
  request.mockImplementation(async (path: string) => path === "/api/library" ? { objects: [] }
    : { matches: [hit("input", "user"), hit("phase", "stage"), hit("answer")], nextCursor: null });
  const router = openSearch();
  expect(request.mock.calls.filter(([path]) => path.includes("sessions/search"))).toHaveLength(0);
  await user.type(screen.getByRole("combobox"), "权限");
  const option = await screen.findByRole("option", { name: /Conversation input/ });
  expect(option.textContent).toContain("searchOverlay.match.user");
  expect(option.textContent).toContain("权限核对完成");
  expect(screen.getByRole("option", { name: /Conversation phase/ }).textContent).toContain("searchOverlay.match.stage");
  expect(screen.getByRole("option", { name: /Conversation answer/ }).textContent).toContain("searchOverlay.match.final");
  expect(within(option).getByText("权限", { selector: "mark" })).toBeTruthy();
  await user.keyboard("{ArrowDown}{Enter}");
  await waitFor(() => expect(router.state.location.pathname).toBe("/w/research/sessions/phase"));
});

test("a late result from an earlier query cannot replace the current results", async () => {
  const user = userEvent.setup();
  let completeOld!: (response: unknown) => void;
  request.mockImplementation(async (path: string) => {
    if (path === "/api/library") return { objects: [] };
    const query = new URL(path, "http://localhost").searchParams.get("query");
    if (query === "old") return new Promise(resolve => { completeOld = resolve; });
    return { matches: [hit("new", "final", "new answer")], nextCursor: null };
  });
  openSearch();
  const input = screen.getByRole("combobox");
  await user.type(input, "old");
  await waitFor(() => expect(completeOld).toBeTypeOf("function"));
  await user.clear(input); await user.type(input, "new");
  await screen.findByRole("option", { name: /Conversation new/ });
  await act(async () => { completeOld({ matches: [hit("old")], nextCursor: null }); });
  await waitFor(() => expect(screen.getAllByRole("option")).toHaveLength(1));
  expect(screen.queryByRole("option", { name: /Conversation old/ })).toBeNull();
});

test("more results use the same query cursor and errors retain the query for retry", async () => {
  const user = userEvent.setup();
  let failed = false;
  request.mockImplementation(async (path: string) => {
    if (path === "/api/library") return { objects: [] };
    if (path.includes("cursor=")) {
      if (!failed) { failed = true; throw new Error("offline"); }
      return { matches: [hit("second")], nextCursor: null };
    }
    return { matches: [hit("first")], nextCursor: "cursor-1" };
  });
  openSearch();
  const input = screen.getByRole("combobox") as HTMLInputElement;
  await user.type(input, "权限");
  await screen.findByRole("option", { name: /Conversation first/ });
  await user.click(screen.getByRole("button", { name: "searchOverlay.loadMoreConversations" }));
  expect((await screen.findByRole("alert")).textContent).toBe("searchOverlay.conversationsLoadFailed");
  expect(input.value).toBe("权限");
  expect(screen.getByRole("option", { name: /Conversation first/ })).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "searchOverlay.loadMoreConversations" }));
  await screen.findByRole("option", { name: /Conversation second/ });
  expect(screen.getAllByRole("option")).toHaveLength(2);
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.queryByRole("button", { name: "searchOverlay.loadMoreConversations" })).toBeNull();
});
