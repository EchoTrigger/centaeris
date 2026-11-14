// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { createMemoryRouter, RouterProvider } from "react-router";
import { configureApi, clearCsrfToken } from "../../src/api";
import { ShellSidebar } from "../../src/shell/ShellSidebar";
import { useSessionNavigationActions } from "../../src/shell/useSessionNavigationActions";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { i18n } from "../../src/i18n";
import { ChatTitle } from "../../src/shell/ChatTitle";
import { readChatMarkdown } from "../../src/chat/chatMarkdown";

vi.mock("../../src/chat/chatMarkdown", () => ({ readChatMarkdown: vi.fn() }));
beforeEach(async () => {
  await i18n.changeLanguage("en");
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
});
afterEach(() => { cleanup(); clearCsrfToken(); vi.unstubAllGlobals(); vi.resetAllMocks(); });
const session = { id: "s", title: "Research notes", isPinned: false };
function mount() {
  const actions = { renameSession: vi.fn(), onTogglePin: vi.fn(), onArchive: vi.fn() };
  render(<ChatTitle session={session} actions={actions} titleControl={<button>Research notes</button>} />);
  return actions;
}

test("favorite stays beside the title; rename and archive use existing actions and restore focus", async () => {
  const user = userEvent.setup(); const actions = mount();
  await user.click(screen.getByRole("button", { name: "Favorite chat" }));
  expect(actions.onTogglePin).toHaveBeenCalledWith(session);
  const more = screen.getByRole("button", { name: "Chat actions for Research notes" });
  await user.click(more);
  await user.click(await screen.findByRole("menuitem", { name: "Rename" }));
  const input = await screen.findByRole("textbox");
  await user.clear(input); await user.type(input, "Edited{Enter}");
  expect(actions.renameSession).toHaveBeenCalledWith(session, "Edited");
  await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("button", { name: "Chat actions for Research notes" })));
  await user.click(screen.getByRole("button", { name: "Chat actions for Research notes" }));
  await user.click(await screen.findByRole("menuitem", { name: "Archive" }));
  expect(actions.onArchive).toHaveBeenCalledWith(session);
});

test("copy reports failure in the menu and retries without writing partial content", async () => {
  const user = userEvent.setup(); mount();
  const clipboard = vi.spyOn(navigator.clipboard, "writeText");
  vi.mocked(readChatMarkdown).mockRejectedValueOnce(new Error("denied")).mockResolvedValueOnce("# Research notes\n\nComplete messages\n");
  await user.click(screen.getByRole("button", { name: "Chat actions for Research notes" }));
  await user.click(await screen.findByRole("menuitem", { name: "Copy as Markdown" }));
  expect(await screen.findByRole("alert")).toBeTruthy();
  expect(clipboard).not.toHaveBeenCalled();
  await user.click(screen.getByRole("menuitem", { name: "Copy as Markdown" }));
  expect(await screen.findByRole("menuitem", { name: "Copied" })).toBeTruthy();
  expect(clipboard).toHaveBeenCalledWith("# Research notes\n\nComplete messages\n");
});


test("favoriting a chat saves its metadata and adds a working sidebar entry until unfavorited", async () => {
  configureApi({ apiBaseUrl: window.location.origin });
  const user = userEvent.setup();
  let stored = { ...session, workspaceId: "demo", isUnread: false, isManuallyUnread: false, latestAttentionSequence: "0", readThroughSequence: "0", attentionRevision: "0" };
  const writes: unknown[] = [];
  vi.stubGlobal("fetch", async (url: string, options?: RequestInit) => {
    if (url.endsWith("/api/csrf")) return Response.json({ csrfToken: "test-csrf" });
    if (url.endsWith("/api/sessions/s") && options?.method === "PATCH") {
      const update = JSON.parse(String(options.body));
      writes.push(update); stored = { ...stored, ...update };
      return Response.json({ session: stored });
    }
    throw new Error(`Unexpected request: ${url}`);
  });
  function Fixture() {
    const [sessions, setSessions] = useState([stored]);
    const actions = useSessionNavigationActions({ scopeKey: "demo", sessions, onChangeSessions: setSessions });
    return <><ShellSidebar workspace={{ id: "demo", name: "Demo" }} sessionProps={{ sessions }} />
      <ChatTitle session={sessions[0]} actions={actions} titleControl={<button>{session.title}</button>} /></>;
  }
  function open() {
    const router = createMemoryRouter([{ id: "authenticated", loader: () => ({ user: { id: "owner" } }), HydrateFallback: () => null, children: [{ id: "workspace", path: "/w/demo/*", loader: () => ({ workspaces: [] }), Component: Fixture }] }], { initialEntries: ["/w/demo/app"] });
    render(<RouterProvider router={router} />);
    return router;
  }
  open();
  await user.click(await screen.findByRole("button", { name: "Favorite chat" }));
  expect(await screen.findByRole("link", { name: session.title })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Unfavorite chat" }).getAttribute("aria-pressed")).toBe("true");
  expect(writes).toEqual([{ isPinned: true }]);
  cleanup();
  const router = open();
  await user.click(await screen.findByRole("link", { name: session.title }));
  expect(router.state.location.pathname).toBe("/w/demo/sessions/s");
  await user.click(screen.getByRole("button", { name: "Unfavorite chat" }));
  await waitFor(() => expect(screen.queryByRole("link", { name: session.title })).toBeNull());
  expect(writes).toEqual([{ isPinned: true }, { isPinned: false }]);
});
