// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { createMemoryRouter, RouterProvider, Outlet } from "react-router";
import { WorkspaceSearch, useWorkspaceSearch } from "../../src/shell/WorkspaceSearch";
import { WorkspaceSearchHistory } from "../../src/shell/workspaceSearchState";

const request = vi.hoisted(() => vi.fn());
vi.mock("../../src/api", () => ({ apiJson: (...args: unknown[]) => request(...args) }));
vi.mock("../../src/i18n", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));

const hit = (id: string, kind = "final", snippet = "权限核对完成") => ({
  sessionId: id, title: `Conversation ${id}`, updatedAt: "2026-11-11T00:00:00Z",
  match: { kind, snippet, eventId: `event-${id}`, messageId: `message-${id}`, sourceSequence: "3" },
});

beforeEach(() => {
  request.mockReset();
  request.mockImplementation(async (path: string) => !path.includes("sessions/search")
    ? { matches: [], nextCursor: null } : { matches: [], nextCursor: null });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

function openSearch() {
  const router = createMemoryRouter([
    { path: "/", element: <WorkspaceSearch workspaceId="research" onClose={() => {}} /> },
    { path: "/w/research/sessions/:id", element: <div>Selected conversation</div> },
    { path: "/w/research/library", element: <div>Material page</div> },
  ]);
  render(<RouterProvider router={router} />);
  return router;
}

test("body hits show their type and snippet and open the selected conversation by keyboard", async () => {
  const user = userEvent.setup();
  request.mockImplementation(async (path: string) => !path.includes("sessions/search") ? { matches: [], nextCursor: null }
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
    if (!path.includes("sessions/search")) return { matches: [], nextCursor: null };
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
    if (!path.includes("sessions/search")) return { matches: [], nextCursor: null };
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

test("browser history restores the query, loaded pages, selection and scroll position", async () => {
  const user = userEvent.setup();
  let failFirst = true;
  request.mockImplementation(async (path: string) => {
    if (!path.includes("sessions/search")) return { matches: [], nextCursor: null };
    if (failFirst) { failFirst = false; throw new Error("offline"); }
    return path.includes("cursor=") ? { matches: [hit("second")], nextCursor: null }
      : { matches: [hit("first")], nextCursor: "next" };
  });
  function SearchPage() {
    const search = useWorkspaceSearch("page");
    return <><button type="button" onClick={search.toggle}>Open search</button>
      {search.open ? <WorkspaceSearch workspaceId="research" onClose={search.close} inputRef={search.inputRef} /> : null}</>;
  }
  const router = createMemoryRouter([{ element: <WorkspaceSearchHistory><Outlet /></WorkspaceSearchHistory>, children: [
    { path: "/", element: <SearchPage /> },
    { path: "/w/research/sessions/:id", element: <div>Selected conversation</div> },
  ] }]);
  render(<RouterProvider router={router} />);
  await user.click(screen.getByRole("button", { name: "Open search" }));
  await user.type(screen.getByRole("combobox"), "权限");
  await screen.findByRole("alert");
  await user.click(screen.getByRole("button", { name: "agentChat.retry" }));
  await screen.findByRole("option", { name: /Conversation first/ });
  await user.click(screen.getByRole("button", { name: "searchOverlay.loadMoreConversations" }));
  const second = await screen.findByRole("option", { name: /Conversation second/ });
  const list = screen.getByRole("listbox");
  list.scrollTop = 137;
  await user.click(second);
  expect(new URLSearchParams(router.state.location.search).get("at")).toBe("event-second");
  const requestCount = request.mock.calls.length;
  await act(async () => { await router.navigate(-1); });
  expect((await screen.findByRole("combobox")) as HTMLInputElement).toHaveProperty("value", "权限");
  await screen.findByRole("option", { name: /Conversation second/ });
  expect(screen.getAllByRole("option")).toHaveLength(2);
  expect(screen.getByRole("option", { name: /Conversation second/ }).getAttribute("aria-selected")).toBe("true");
  expect(screen.getByRole("listbox").scrollTop).toBe(137);
  expect(request.mock.calls.length).toBeGreaterThan(requestCount);
  await act(async () => { await router.navigate(1); });
  expect(new URLSearchParams(router.state.location.search).get("at")).toBe("event-second");
  await act(async () => { await router.navigate(-1); await router.navigate(-1); });
  expect(router.state.location.hash).toBe("");
  expect(screen.queryByRole("combobox")).toBeNull();
});

test("leaving during pagination cancels the request and Back restores usable results", async () => {
  const user = userEvent.setup();
  let pageSignal!: AbortSignal;
  let finishPage!: (value: unknown) => void;
  request.mockImplementation(async (path: string, options: { signal: AbortSignal }) => {
    if (!path.includes("sessions/search")) return { matches: [], nextCursor: null };
    if (path.includes("cursor=")) { pageSignal = options.signal; return new Promise(resolve => { finishPage = resolve; }); }
    return { matches: [hit("first")], nextCursor: "next" };
  });
  const router = createMemoryRouter([{ element: <WorkspaceSearchHistory><Outlet /></WorkspaceSearchHistory>, children: [
    { path: "/", element: <WorkspaceSearch workspaceId="research" onClose={() => {}} /> },
    { path: "/w/research/sessions/:id", element: <div>Selected conversation</div> },
  ] }]);
  render(<RouterProvider router={router} />);
  await user.type(screen.getByRole("combobox"), "权限");
  await screen.findByRole("option", { name: /Conversation first/ });
  await user.click(screen.getByRole("button", { name: "searchOverlay.loadMoreConversations" }));
  await user.click(screen.getByRole("option", { name: /Conversation first/ }));
  expect(pageSignal.aborted).toBe(true);
  await act(async () => { await router.navigate(-1); });
  expect((await screen.findByRole("button", { name: "searchOverlay.loadMoreConversations" })).hasAttribute("disabled")).toBe(false);
  await act(async () => { finishPage({ matches: [hit("late")], nextCursor: null }); });
  expect(screen.queryByRole("option", { name: /Conversation late/ })).toBeNull();
  expect(screen.getAllByRole("option")).toHaveLength(1);
});


test("authorized material and submitted-result body hits use the server's original-content links", async () => {
  const user = userEvent.setup();
  request.mockImplementation(async (path: string) => path.includes("sessions/search") ? { matches: [], nextCursor: null } : {
    matches: [
      { id: "sourceObject:rules", kind: "material", title: "Rules.pdf", snippet: "差旅权限材料", href: "/w/research/library?view=knowledge&source=rules&object=source&offset=10" },
      { id: "deliverable:report", kind: "deliverable", title: "Submitted report", snippet: "权限核查结论", href: "/w/research/work/tasks/issue?deliverable=report" },
    ], nextCursor: null,
  });
  const router = openSearch();
  await user.type(screen.getByRole("combobox"), "权限");
  const material = await screen.findByRole("option", { name: /Rules.pdf/ });
  expect(material.textContent).toContain("searchOverlay.match.material");
  expect(screen.getByRole("option", { name: /Submitted report/ }).textContent).toContain("searchOverlay.match.deliverable");
  await user.click(material);
  expect(router.state.location.pathname).toBe("/w/research/library");
  expect(router.state.location.search).toContain("offset=10");
});

test("returning to search reauthorizes results before showing revoked content", async () => {
  const user = userEvent.setup(); let allowed = true;
  request.mockImplementation(async (path: string) => ({ matches: !path.includes("sessions/search") && allowed
    ? [{ id: "sourceObject:private", kind: "material", title: "Restricted material", snippet: "private rule", href: "/w/research/library/private" }] : [], nextCursor: null }));
  const router = createMemoryRouter([{ element: <WorkspaceSearchHistory><Outlet /></WorkspaceSearchHistory>, children: [
    { path: "/", element: <WorkspaceSearch workspaceId="research" onClose={() => {}} /> },
    { path: "/w/research/library/private", element: <div>Material page</div> },
  ] }]);
  render(<RouterProvider router={router} />);
  await user.type(screen.getByRole("combobox"), "private");
  await user.click(await screen.findByRole("option", { name: /Restricted material/ }));
  allowed = false;
  await act(async () => { await router.navigate(-1); });
  expect(screen.queryByRole("option", { name: /Restricted material/ })).toBeNull();
  await waitFor(() => expect(screen.queryByText("agentChat.loading")).toBeNull());
  expect(screen.queryByRole("option", { name: /Restricted material/ })).toBeNull();
});
