// @vitest-environment jsdom
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router";
import { FrontendProvider } from "@centaeris/ui/provider";
import { clearCsrfToken, configureApi } from "../../src/api";
import { i18n } from "../../src/i18n";
import { WorkHome } from "../../src/work/WorkHome";

let dismissed = false;
let rejectSave = false;
let steps = [{ id: "materials", completed: false }, { id: "conversation", completed: false }, { id: "issue", completed: true }];
beforeEach(async () => {
  dismissed = false; rejectSave = false;
  clearCsrfToken(); configureApi({ apiBaseUrl: "http://localhost" }); await i18n.changeLanguage("en");
  vi.stubGlobal("fetch", vi.fn(async (input, options) => {
    if (String(input).endsWith("/api/csrf")) return Response.json({ csrfToken: "token" });
    if (options?.method === "PATCH") {
      if (rejectSave) return Response.json({ error: "guide_save_failed" }, { status: 500 });
      dismissed = JSON.parse(options.body).dismissed;
    }
    return Response.json({ guide: { dismissed, steps } });
  }));
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });
function mount() {
  render(<FrontendProvider><MemoryRouter><WorkHome workspace={{ id: "one", name: "Workspace" }} /></MemoryRouter></FrontendProvider>);
}
test("guide removal waits for the server, survives remount and can be explicitly restored from Help", async () => {
  const user = userEvent.setup(); mount();
  await screen.findByRole("region", { name: "Get started" });
  rejectSave = true;
  await user.click(screen.getByRole("button", { name: "Remove guide" }));
  await screen.findByRole("alert");
  expect(screen.getByRole("region", { name: "Get started" })).toBeTruthy();
  rejectSave = false;
  await user.click(screen.getByRole("button", { name: "Remove guide" }));
  await waitFor(() => expect(screen.queryByRole("region", { name: "Get started" })).toBeNull());
  cleanup(); mount();
  await user.click(screen.getByRole("button", { name: "Help" }));
  expect(screen.queryByRole("region", { name: "Get started" })).toBeNull();
  await user.click(await screen.findByRole("menuitem", { name: "Getting started guide" }));
  await screen.findByRole("region", { name: "Get started" });
  expect(dismissed).toBe(false);
});
test("guide uses authorized steps and real completion receipts without creating business records", async () => {
  steps = [{ id: "materials", completed: false }, { id: "conversation", completed: false }, { id: "issue", completed: true }];
  mount();
  const guide = within(await screen.findByRole("region", { name: "Get started" }));
  expect(guide.getAllByRole("link").map(link => link.getAttribute("href"))).toEqual(["/w/one/library", "/w/one/app", "/w/one/work/mine"]);
  expect(guide.getAllByLabelText("Completed")).toHaveLength(1);
  expect(guide.queryByText("Configure a model")).toBeNull();
  expect(vi.mocked(fetch).mock.calls.every(([, options]) => !options?.method || options.method === "GET")).toBe(true);
});
