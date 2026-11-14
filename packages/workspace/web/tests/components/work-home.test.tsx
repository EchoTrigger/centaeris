// @vitest-environment jsdom
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import { configureApi } from "../../src/api";
import { i18n } from "../../src/i18n";
import { WorkHome } from "../../src/work/WorkHome";

beforeEach(async () => {
  configureApi({ apiBaseUrl: "http://localhost" });
  await i18n.changeLanguage("en");
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

function mount(templates = []) {
  vi.stubGlobal("fetch", vi.fn(async () => Response.json({ templates })));
  render(<MemoryRouter><WorkHome workspace={{ id: "one", name: "Workspace" }} /></MemoryRouter>);
}

test("home keeps workspace resources available without the getting-started guide", async () => {
  mount();
  expect(screen.getByRole("heading", { name: "Workspace", level: 2 })).toBeTruthy();
  const resources = within(screen.getByRole("region", { name: "Workspace resources" }));
  expect(resources.getAllByRole("link").map(link => link.getAttribute("href"))).toEqual([
    "/w/one/library", "/w/one/extensions?view=connections", "/w/one/extensions?view=skills", "/w/one/work/projects",
  ]);
  expect(screen.queryByRole("button", { name: "Help" })).toBeNull();
  expect(screen.queryByRole("region", { name: "Get started" })).toBeNull();
  await waitFor(() => expect(fetch).toHaveBeenCalledTimes(1));
  expect(vi.mocked(fetch).mock.calls.map(([input]) => String(input))).toEqual(["http://localhost/api/workspaces/one/issue-templates"]);
});

test("home retains links for other enabled workspace templates", async () => {
  mount([{ key: "document-review", name: "Document review" }]);
  const template = await screen.findByRole("link", { name: "Document review" });
  expect(template.getAttribute("href")).toBe("/w/one/work/tasks/new?template=document-review");
  expect(screen.getByRole("region", { name: "Workspace resources" })).toBeTruthy();
});
