// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
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
afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.resetAllMocks(); });
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
