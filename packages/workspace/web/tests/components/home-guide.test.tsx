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
let releaseSave: (() => void) | null = null;
let delaySave = false;
let steps = [{ id: "materials", completed: false }, { id: "conversation", completed: false }, { id: "issue", completed: true }];
beforeEach(async () => {
  dismissed = false; rejectSave = false; delaySave = false; releaseSave = null;
  steps = [{ id: "materials", completed: false }, { id: "conversation", completed: false }, { id: "issue", completed: true }];
  clearCsrfToken(); configureApi({ apiBaseUrl: "http://localhost" }); await i18n.changeLanguage("en");
  vi.stubGlobal("fetch", vi.fn(async (input, options) => {
    if (String(input).endsWith("/api/csrf")) return Response.json({ csrfToken: "token" });
    if (options?.method === "PATCH") {
      if (delaySave) await new Promise<void>(resolve => { releaseSave = resolve; });
      if (rejectSave) return Response.json({ error: "guide_save_failed" }, { status: 500 });
      const change = JSON.parse(options.body);
      if ("dismissed" in change) dismissed = change.dismissed;
      if (change.manualSteps) steps = steps.map(step => ({ ...step, completed: change.manualSteps[step.id] ?? step.completed }));
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
  expect(guide.getAllByRole("checkbox", { checked: true })).toHaveLength(1);
  expect(guide.queryByText("Configure a model")).toBeNull();
  expect(vi.mocked(fetch).mock.calls.every(([, options]) => !options?.method || options.method === "GET")).toBe(true);
});

test("manual choices use acknowledged saves, keyboard controls and persist after remount", async () => {
  const user = userEvent.setup(); mount();
  const guide = within(await screen.findByRole("region", { name: "Get started" }));
  const checkbox = guide.getAllByRole("checkbox")[2];
  delaySave = true;
  checkbox.focus();
  await user.keyboard(" ");
  await waitFor(() => expect(releaseSave).not.toBeNull());
  expect(checkbox.getAttribute("aria-checked")).toBe("true");
  expect(checkbox.hasAttribute("disabled")).toBe(true);
  releaseSave?.();
  await waitFor(() => expect(checkbox.getAttribute("aria-checked")).toBe("false"));
  expect(guide.getAllByRole("link")[2].getAttribute("href")).toBe("/w/one/work/tasks/new");
  expect(checkbox.closest("a")).toBeNull();
  cleanup(); mount();
  const restored = within(await screen.findByRole("region", { name: "Get started" })).getAllByRole("checkbox")[2];
  expect(restored.getAttribute("aria-checked")).toBe("false");
  delaySave = false;
  restored.focus();
  await user.keyboard("{Enter}");
  await waitFor(() => expect(restored.getAttribute("aria-checked")).toBe("true"));
  const writes = vi.mocked(fetch).mock.calls.filter(([, options]) => options?.method === "PATCH");
  expect(writes.map(([, options]) => JSON.parse(options!.body as string))).toEqual([
    { manualSteps: { issue: false } }, { manualSteps: { issue: true } },
  ]);
});

test("failed manual save keeps acknowledged completion and allows another attempt", async () => {
  const user = userEvent.setup(); mount();
  const guide = within(await screen.findByRole("region", { name: "Get started" }));
  const checkbox = guide.getAllByRole("checkbox")[0];
  rejectSave = true;
  await user.click(checkbox);
  await screen.findByRole("alert");
  expect(checkbox.getAttribute("aria-checked")).toBe("false");
  expect(checkbox.hasAttribute("disabled")).toBe(false);
  rejectSave = false;
  await user.click(checkbox);
  await waitFor(() => expect(checkbox.getAttribute("aria-checked")).toBe("true"));
  expect(screen.queryByRole("alert")).toBeNull();
});
