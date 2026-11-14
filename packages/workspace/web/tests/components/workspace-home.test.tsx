// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState, type ReactNode } from "react";
import { createMemoryRouter, RouterProvider, useRouteLoaderData } from "react-router";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { clearCsrfToken, configureApi } from "../../src/api";
import SettingsRoute from "../../src/routes/SettingsRoute";
import { SessionArchive } from "../../src/routes/SessionArchive";
import WorkspaceHomeRoute from "../../src/routes/WorkspaceHomeRoute";
import { HomePlane } from "../../src/shell/HomePlane";
import { ShellSidebar } from "../../src/shell/ShellSidebar";
import { useWorkspaceSearch, WorkspaceSearch } from "../../src/shell/WorkspaceSearch";
import { THEME_STORAGE_KEY } from "../../src/theme";

vi.mock("../../src/i18n", () => {
  const t = (key: string) => key;
  return { t, useTranslation: () => ({ t, i18n: { resolvedLanguage: "en" } }) };
});

vi.mock("../../src/routes/AppRoute", () => ({
  AppPageContent({ location, initialSession, visible, catalogContent, chatFloating, onShowChat, onMinimizeChat, onExpandChat, onDockChat }: { location: { key: string, pathname: string, search: string }, initialSession?: { id: string } | null, visible: boolean, catalogContent?: ReactNode, chatFloating?: boolean, onShowChat?: () => void, onMinimizeChat?: () => void, onExpandChat?: () => void, onDockChat?: () => void }) {
    const { workspace } = useRouteLoaderData("workspace");
    const [draft, setDraft] = useState("");
    const search = useWorkspaceSearch(location.key, visible);
    return <>
      <ShellSidebar workspace={workspace} agentPath={location.pathname + location.search} search={search} />
      <output aria-label="Conversation session">{initialSession?.id || "draft"}</output>
      <textarea id="messageDraft" hidden={!visible || search.open} aria-label="Session draft" value={draft} onChange={event => setDraft(event.target.value)} />
      {catalogContent}
      <button onClick={onShowChat}>打开浮窗</button><button onClick={onMinimizeChat}>最小化会话</button><button onClick={onExpandChat}>展开会话</button><button onClick={onDockChat}>收起到工具栏</button>
      {chatFloating ? <span>浮窗已打开</span> : null}
      {search.open ? <WorkspaceSearch workspaceId={workspace.id} onClose={search.close} inputRef={search.inputRef} /> : null}
    </>;
  },
}));

beforeEach(() => {
  configureApi({ apiBaseUrl: window.location.origin });
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  clearCsrfToken();
  delete document.documentElement.dataset.themePreference;
  localStorage.removeItem(THEME_STORAGE_KEY);
});

test("password editing keeps validation in the dialog and clears a cancelled draft", async () => {
  const user = userEvent.setup();
  const writes: unknown[] = [];
  vi.stubGlobal("fetch", async (url: string, options?: RequestInit) => {
    if (url.endsWith("/api/csrf")) return Response.json({ csrfToken: "csrf-token" });
    if (url.endsWith("/api/account/external-identities")) return Response.json({ providers: [] });
    if (url.endsWith("/api/account/password")) {
      writes.push(JSON.parse(String(options?.body)));
      return Response.json({});
    }
    throw new Error(`Unexpected request: ${url}`);
  });
  const router = createMemoryRouter([{
    id: "authenticated",
    loader: () => ({ user: { id: "owner" } }),
    HydrateFallback: () => null,
    children: [{ path: "settings/security", Component: SettingsRoute }],
  }], { initialEntries: ["/settings/security"] });
  render(<RouterProvider router={router} />);
  const edit = await screen.findByRole("button", { name: "passwordResetRoute.updatePassword" });
  expect(screen.queryByLabelText("settingsRoute.currentPassword")).toBeNull();
  await user.click(edit);
  let dialog = within(screen.getByRole("dialog", { name: "passwordResetRoute.updatePassword" }));
  await user.type(dialog.getByLabelText("settingsRoute.currentPassword"), "discarded password");
  await user.keyboard("{Escape}");
  expect(screen.queryByRole("dialog")).toBeNull();
  await user.click(edit);
  dialog = within(screen.getByRole("dialog", { name: "passwordResetRoute.updatePassword" }));
  const current = dialog.getByLabelText("settingsRoute.currentPassword") as HTMLInputElement;
  expect(current.value).toBe("");
  await user.type(current, "current password");
  await user.type(dialog.getByLabelText("passwordResetRoute.newPassword"), "new test password");
  const confirmation = dialog.getByLabelText("settingsRoute.confirmNewPassword");
  await user.type(confirmation, "different password");
  await user.click(dialog.getByRole("button", { name: "passwordResetRoute.updatePassword" }));
  expect(dialog.getByRole("alert").textContent).toBe("settingsRoute.theNewPasswordsDoNotMatch");
  expect(writes).toEqual([]);
  await user.clear(confirmation);
  await user.type(confirmation, "new test password");
  await user.click(dialog.getByRole("button", { name: "passwordResetRoute.updatePassword" }));
  expect(await screen.findByText("settingsRoute.passwordUpdatedThisDeviceStaysSignedInOtherSessions")).toBeTruthy();
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(writes).toEqual([{ currentPassword: "current password", newPassword: "new test password" }]);
});

test("archive deletion confirms its scope and retries only conversations that failed", async () => {
  const user = userEvent.setup();
  const workspace = { id: "research", role: "owner" };
  const removed: string[] = [];
  let failOnce = true;
  vi.stubGlobal("fetch", async (url: string, options: RequestInit) => {
    const path = new URL(url).pathname;
    if (path.endsWith("/archived-sessions")) return Response.json({ sessions: [
      { id: "a", title: "Design notes" }, { id: "b", title: "Draft brief" }, { id: "c", title: "Planning call" },
    ] });
    if (path.endsWith("/csrf")) return Response.json({ csrfToken: "session-csrf" });
    if (options.method === "DELETE") {
      removed.push(path);
      if (path === "/api/sessions/c" && failOnce) {
        failOnce = false;
        return Response.json({ error: "runtime_unavailable" }, { status: 503 });
      }
      return Response.json({ deleted: true });
    }
    throw new Error("Unexpected request: " + path);
  });
  const router = createMemoryRouter([{ path: "/", element: <SessionArchive workspace={workspace} returnTo="/app" /> }]);
  render(<RouterProvider router={router} />);
  const title = await screen.findByText("Design notes");
  const row = title.closest("li")!;
  expect(within(row).getByRole("button", { name: "archive.restoreTitle" })).toBeTruthy();
  await user.click(within(row).getByRole("button", { name: "archive.deleteTitle" }));
  const dialog = await screen.findByRole("dialog", { name: "appRoute.deleteThisConversation" });
  expect(removed).toEqual([]);
  await user.click(within(dialog).getByRole("button", { name: "agentRunRow.cancel" }));
  expect(screen.getByText("Design notes")).toBeTruthy();

  await user.click(within(row).getByRole("button", { name: "archive.deleteTitle" }));
  await user.click(within(await screen.findByRole("dialog", { name: "appRoute.deleteThisConversation" })).getByRole("button", { name: "appRoute.delete" }));
  expect(screen.queryByText("Design notes")).toBeNull();
  expect(screen.getByText("Draft brief")).toBeTruthy();
  expect(removed).toEqual(["/api/sessions/a"]);

  await user.click(screen.getByRole("button", { name: "archive.deleteAll" }));
  const bulk = await screen.findByRole("dialog", { name: "archive.confirmDeleteAll" });
  expect(removed).toEqual(["/api/sessions/a"]);
  await user.click(within(bulk).getByRole("button", { name: "appRoute.delete" }));
  await within(bulk).findByText("archive.bulkDeleteFailed");
  expect(screen.queryByText("Draft brief")).toBeNull();
  expect(screen.getByText("Planning call")).toBeTruthy();
  await user.click(within(bulk).getByRole("button", { name: "appRoute.delete" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(screen.queryByRole("list")).toBeNull();
  expect(screen.getByRole("heading", { name: "archive.title" })).toBeTruthy();
  expect(removed).toEqual(["/api/sessions/a", "/api/sessions/b", "/api/sessions/c", "/api/sessions/c"]);
});

test("the welcome area shows the product mark and a time-of-day greeting", () => {
  const { container } = render(<HomePlane />);
  expect(container.querySelector("img")?.getAttribute("src")).toBe("/centaeris-mark.png");
  expect(screen.getByRole("heading", { level: 1 }).textContent).toMatch(/^homePlane\.greeting\.(morning|afternoon|evening)$/);
});

test("workspace search reports a failed source and can retry the same query", async () => {
  const user = userEvent.setup();
  let libraryRequests = 0;
  vi.stubGlobal("fetch", async (url: string) => {
    if (url.includes("/workspaces/research/search?")) {
      libraryRequests += 1;
      if (libraryRequests === 1) return new Response(null, { status: 503 });
      return Response.json({ matches: [{ id: "userLibraryObject:guide", kind: "material", title: "Field guide", snippet: "", href: "/w/research/library/guide" }], nextCursor: null });
    }
    return Response.json({ matches: [], nextCursor: null });
  });
  const router = createMemoryRouter([{
    path: "/",
    element: <WorkspaceSearch workspaceId="research" onClose={() => {}} />,
  }]);
  render(<RouterProvider router={router} />);

  const input = screen.getByRole("combobox", { name: "searchOverlay.searchConversationsAndNotes" });
  await user.type(input, "field");
  expect((await screen.findByRole("alert")).textContent).toBe("searchOverlay.libraryLoadFailed");
  expect((input as HTMLInputElement).value).toBe("field");
  await user.click(screen.getByRole("button", { name: "agentChat.retry" }));
  expect(await screen.findByRole("option", { name: "Field guide" })).toBeTruthy();
  expect((input as HTMLInputElement).value).toBe("field");
});

test("workspace settings replace the work navigation and return to the same draft", async () => {
  const user = userEvent.setup();
  const workspace = { id: "research", name: "Research", role: "owner", avatarUrl: null };
  const anotherWorkspace = { ...workspace, id: "design", name: "Design" };
  vi.stubGlobal("fetch", async (url: string) => {
    if (url.endsWith("/archived-sessions")) return Response.json({ sessions: [{ id: "archived", title: "Archived conversation" }] });
    if (url.includes("/trash?")) return Response.json({ items: [{ id: "deleted", kind: "library", title: "Deleted file" }], hasMore: false });
    if (url.endsWith("/session-projects")) return Response.json({ projects: [] });
    if (url.endsWith("/api-credentials")) return Response.json({ credentials: [] });
    return Response.json({ objects: [], sessions: [] });
  });
  const router = createMemoryRouter([{
    id: "authenticated",
    loader: () => ({ user: { id: "owner" } }),
    HydrateFallback: () => null,
    children: [{
      id: "workspace",
      path: "w/:workspaceId",
      loader: () => ({ workspace, workspaces: [workspace, anotherWorkspace], selectedSession: null }),
      Component: WorkspaceHomeRoute,
      children: [
        { path: "app", element: <></> },
        { path: "work/*", element: <h1>Work prototype</h1> },
        { path: "library", element: <h1>Library contents</h1> },
        { path: "extensions", element: <h1>Extension catalog</h1> },
        { path: "settings/preferences", Component: SettingsRoute },
        { path: "settings/general", Component: SettingsRoute },
        { path: "settings/archive", Component: SettingsRoute },
        { path: "settings/developer", Component: SettingsRoute },
      ],
    }],
  }], { initialEntries: ["/w/research/app?projectId=example"] });
  render(<RouterProvider router={router} />);

  await user.type(await screen.findByRole("textbox", { name: "Session draft" }), "Keep this draft");
  await user.click(screen.getByRole("link", { name: "work.myIssues" }));
  expect(await screen.findByRole("heading", { name: "Work prototype" })).toBeTruthy();
  await router.navigate(-1);
  expect((await screen.findByRole("textbox", { name: "Session draft" }) as HTMLTextAreaElement).value).toBe("Keep this draft");
  await user.click(screen.getByRole("button", { name: "shellSidebar.newConversation" }));
  expect(screen.getByRole("textbox", { name: "Session draft" })).toBe(document.activeElement);
  screen.getByRole("button", { name: "shellSidebar.valueWorkspaceMenu" }).focus();
  await user.keyboard("{Enter}");
  const workspaceActions = await screen.findByRole("menu", { name: "shellSidebar.valueWorkspaceActions" });
  expect(screen.queryByRole("menuitem", { name: /Design/ })).toBeNull();
  const switchWorkspace = within(workspaceActions).getByRole("menuitem", { name: "shellSidebar.switchWorkspace", expanded: false });
  switchWorkspace.focus();
  await user.keyboard("{ArrowRight}");
  const workspaceChoices = await screen.findByRole("menu", { name: "shellSidebar.switchWorkspace" });
  expect(within(workspaceChoices).getByRole("menuitem", { name: /Design/ }).getAttribute("href")).toBe("/w/design/work");
  await user.keyboard("{ArrowLeft}");
  expect(screen.queryByRole("menu", { name: "shellSidebar.switchWorkspace" })).toBeNull();
  await user.click(within(workspaceActions).getByRole("menuitem", { name: "shellSidebar.settings" }));

  await screen.findByRole("main", { name: "settingsRoute.preferences" });
  const system = screen.getByRole("radio", { name: "theme.system" }) as HTMLInputElement;
  const dark = screen.getByRole("radio", { name: "theme.dark" }) as HTMLInputElement;
  expect(system.checked).toBe(true);
  await user.click(dark);
  expect(dark.checked).toBe(true);
  expect(localStorage.getItem(THEME_STORAGE_KEY)).toBe("dark");
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(screen.queryByRole("complementary", { name: "shellSidebar.conversationNavigation" })).toBeNull();
  expect(screen.queryByRole("button", { name: "shellSidebar.newConversation" })).toBeNull();
  expect(screen.queryByRole("textbox", { name: "Session draft" })).toBeNull();
  expect(fireEvent.keyDown(window, { key: "k", ctrlKey: true })).toBe(true);
  expect(screen.queryByRole("search", { name: "searchOverlay.workspaceSearch" })).toBeNull();

  await user.click(screen.getByRole("link", { name: "pluginSettings.general" }));
  await screen.findByRole("main", { name: "pluginSettings.general" });
  await user.click(screen.getByRole("link", { name: "archive.title" }));
  expect(await screen.findByText("Archived conversation")).toBeTruthy();
  await user.click(screen.getByRole("link", { name: "settingsRoute.recentlyDeleted" }));
  expect(await screen.findByText("Deleted file")).toBeTruthy();
  expect(router.state.location.pathname).toBe("/w/research/settings/archive");
  expect(screen.getByRole("heading", { name: "archive.title" })).toBeTruthy();
  expect(screen.queryByText("Archived conversation")).toBeNull();

  await user.click(screen.getByRole("link", { name: "developer.title" }));
  await user.click(await screen.findByRole("link", { name: "developer.docs" }));
  await screen.findByRole("heading", { name: "developer.doc.quickstart.title" });
  expect(screen.queryByRole("navigation", { name: "shellSidebar.settings" })).toBeNull();
  const backToSettings = screen.getByRole("link", { name: "developer.backToSettings" });
  await user.click(screen.getByRole("link", { name: "developer.doc.errors.title" }));
  expect(router.state.location.search).toBe("?view=docs&topic=errors");
  await router.navigate(-1);
  await screen.findByRole("heading", { name: "developer.doc.quickstart.title" });
  const docs = screen.getByRole("navigation", { name: "developer.docs" });
  await user.click(within(docs).getByRole("link", { name: "developer.doc.events.title" }));
  expect(await screen.findByText("/api/v1/agent/operations/{operationId}")).toBeTruthy();
  expect(screen.getByText("artifactCompleted")).toBeTruthy();
  await user.click(within(docs).getByRole("link", { name: "developer.doc.artifacts.title" }));
  expect(router.state.location.search).toBe("?view=docs&topic=artifacts");
  await user.click(backToSettings);
  await screen.findByRole("heading", { name: "developer.agentRunApi" });
  expect(screen.getByRole("navigation", { name: "shellSidebar.settings" })).toBeTruthy();
  await user.click(screen.getByRole("link", { name: "settingsRoute.backToApp" }));

  expect(router.state.location.search).toBe("?projectId=example");
  expect((await screen.findByRole("textbox", { name: "Session draft" }) as HTMLTextAreaElement).value).toBe("Keep this draft");
  await user.click(screen.getByRole("button", { name: "searchOverlay.searchConversationsAndNotes" }));
  expect(await screen.findByRole("search", { name: "searchOverlay.workspaceSearch" })).toBeTruthy();
  expect(screen.getByRole("combobox", { name: "searchOverlay.searchConversationsAndNotes" })).toBe(document.activeElement);
  expect(screen.getByRole("navigation", { name: "shellSidebar.mainView" })).toBeTruthy();
  await user.keyboard("{Escape}");
  expect(screen.queryByRole("search")).toBeNull();
  expect((screen.getByRole("textbox", { name: "Session draft" }) as HTMLTextAreaElement).value).toBe("Keep this draft");

  const browsingSidebar = screen.getByRole("complementary", { name: "shellSidebar.conversationNavigation" });
  await user.click(screen.getByRole("link", { name: "libraryObjectRoute.library" }));
  expect(await screen.findByRole("heading", { name: "Library contents" })).toBeTruthy();
  expect(screen.getByRole("complementary", { name: "shellSidebar.conversationNavigation" })).toBe(browsingSidebar);
  await user.click(screen.getByRole("link", { name: "extensions.title" }));
  expect(await screen.findByRole("heading", { name: "Extension catalog" })).toBeTruthy();
  expect(screen.getByRole("complementary", { name: "shellSidebar.conversationNavigation" })).toBe(browsingSidebar);
  screen.getByRole("button", { name: "shellSidebar.valueWorkspaceMenu" }).focus();
  await user.keyboard("{Enter}");
  await user.click(await screen.findByRole("menuitem", { name: "shellSidebar.settings" }));
  await user.click(await screen.findByRole("link", { name: "settingsRoute.backToApp" }));
  expect(await screen.findByRole("heading", { name: "Extension catalog" })).toBeTruthy();
});

test("a conversation keeps its draft while floating above work, minimizing and expanding", async () => {
  const user = userEvent.setup();
  const workspace = { id: "dock", name: "Dock" };
  const router = createMemoryRouter([{ id: "authenticated", loader: () => ({ user: { id: "dock-user" } }), HydrateFallback: () => null, children: [{ id: "workspace", path: "/w/dock", loader: () => ({ workspace, selectedSession: null }), Component: WorkspaceHomeRoute, children: [{ path: "app", element: <></> }, { path: "work", element: <h1>工作列表</h1> }] }] }], { initialEntries: ["/w/dock/app"] });
  render(<RouterProvider router={router} />);
  await user.type(await screen.findByRole("textbox", { name: "Session draft" }), "保留这段草稿");
  await user.click(screen.getByRole("link", { name: "work.myIssues" }));
  await screen.findByRole("heading", { name: "工作列表" });
  await user.click(screen.getByRole("button", { name: "打开浮窗" }));
  expect(screen.getByText("浮窗已打开")).toBeTruthy();
  expect((screen.getByRole("textbox", { name: "Session draft" }) as HTMLTextAreaElement).value).toBe("保留这段草稿");
  expect(router.state.location.pathname).toBe("/w/dock/work");
  await user.click(screen.getByRole("button", { name: "最小化会话" }));
  expect(screen.queryByRole("textbox", { name: "Session draft" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "打开浮窗" }));
  await user.click(screen.getByRole("button", { name: "展开会话" }));
  expect(router.state.location.pathname).toBe("/w/dock/app");
  expect((await screen.findByRole("textbox", { name: "Session draft" }) as HTMLTextAreaElement).value).toBe("保留这段草稿");
  await user.click(screen.getByRole("button", { name: "收起到工具栏" }));
  await screen.findByRole("heading", { name: "工作列表" });
  expect(screen.getByText("浮窗已打开")).toBeTruthy();
  expect((screen.getByRole("textbox", { name: "Session draft" }) as HTMLTextAreaElement).value).toBe("保留这段草稿");
});

test("Agent returns to the floating conversation while explicit new conversation still opens a draft", async () => {
  const user = userEvent.setup();
  const workspace = { id: "bound", name: "Bound" };
  const session = { id: "kept-session", title: "Retained conversation" };
  const router = createMemoryRouter([{
    id: "authenticated", loader: () => ({ user: { id: "bound-user" } }), HydrateFallback: () => null,
    children: [{
      id: "workspace", path: "/w/bound", Component: WorkspaceHomeRoute,
      loader: ({ params }) => ({ workspace, selectedSession: params.sessionId === session.id ? session : null }),
      shouldRevalidate: () => true,
      children: [
        { path: "app", element: <></> },
        { path: "sessions/:sessionId", element: <></> },
        { path: "work", element: <h1>工作列表</h1> },
        { path: "work/home", element: <h1>团队主页</h1> },
        { path: "library", element: <h1>资料空间</h1> },
      ],
    }],
  }], { initialEntries: ["/w/bound/sessions/kept-session"] });
  render(<RouterProvider router={router} />);
  expect((await screen.findByLabelText("Conversation session")).textContent).toBe(session.id);
  await user.click(screen.getByRole("button", { name: "收起到工具栏" }));
  await screen.findByRole("heading", { name: "工作列表" });

  for (const destination of ["work.home", "libraryObjectRoute.library"]) {
    await user.click(screen.getByRole("link", { name: "Agent" }));
    expect(router.state.location.pathname).toBe("/w/bound/sessions/kept-session");
    await user.click(screen.getByRole("link", { name: destination }));
    expect(screen.getByText("浮窗已打开")).toBeTruthy();
    expect(screen.getByLabelText("Conversation session").textContent).toBe(session.id);
  }

  await user.click(screen.getByRole("button", { name: "shellSidebar.newConversation" }));
  await waitFor(() => expect(router.state.location.pathname).toBe("/w/bound/app"));
  expect(router.state.location.search).toBe("?new=1");
  expect(screen.getByLabelText("Conversation session").textContent).toBe("draft");
});
