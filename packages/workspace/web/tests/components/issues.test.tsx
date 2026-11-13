// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider, useParams } from "react-router";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { ApiError, apiJson } from "../../src/api";
import { i18n } from "../../src/i18n";
import { Issues } from "../../src/work/Issues";

vi.mock("../../src/api", async importOriginal => ({ ...await importOriginal<typeof import("../../src/api")>(), apiJson: vi.fn() }));
let issue: Record<string, unknown>;
let conflict = false;
let materials: Record<string, unknown>[] = [];
let executions: Record<string, unknown>[] = [];
let requests: { path: string; body: Record<string, unknown> | null }[];
beforeEach(async () => {
  await i18n.changeLanguage("en");
  conflict = false; requests = []; materials = []; executions = [];
  issue = { id: "issue-one", workspaceId: "one", title: "First issue", description: "Real description", status: "todo", assigneeId: "user", creatorId: "user", version: 1, archivedAt: null, createdAt: "2026-11-12T01:00:00Z", updatedAt: "2026-11-12T01:00:00Z" };
  vi.mocked(apiJson).mockImplementation(async (path, options = {}) => {
    const body = options.body ? JSON.parse(String(options.body)) : null;
    requests.push({ path, body });
    if (path.endsWith("/issue-templates")) return { templates: [] };
    if (path.endsWith("/deliverables")) return { version: issue.version, deliverables: [] };
    if (path.endsWith("/materials")) {
      if (options.method === "POST") { materials = [...materials, { id: "attached", ...body, displayName: "Brief.pdf", available: true }]; issue.version = Number(issue.version) + 1; }
      return { materials, version: issue.version };
    }
    if (path === "/api/library") return { objects: [{ id: "file", objectKind: "file", status: "ready", displayName: "Brief.pdf" }] };
    if (path.endsWith("/sources")) return { sources: [] };
    if (path.includes("/executions/execution?")) return { runs: [{ id: "run", status: "completed", summary: "**Delivered analysis**", activity: [{ id: "activity", type: "modelProcessSummary", text: "Read the brief", createdAt: "2026-11-12T01:00:00Z" }], artifacts: [{ id: "artifact", displayName: "Report.pdf", downloadUrl: "/report" }] }], total: 1 };
    if (path.includes("/executions?")) return { executions, total: executions.length };
    if (path.endsWith("/assignees")) return { assignees: [{ id: "user", name: "Taylor" }, { id: "other", name: "Morgan" }] };
    if (path.includes("/events?")) return { events: [{ id: "event", issueId: issue.id, actor: { id: "other", name: "Morgan" }, type: "updated", changes: { status: { before: "backlog", after: "todo" } }, createdAt: "2026-11-12T01:00:00Z" }], total: 1 };
    if (options.method === "PATCH") { if (conflict) throw new ApiError("issue_version_conflict", 409); issue = { ...issue, ...body, version: Number(issue.version) + 1, archivedAt: body.archived === true ? "2026-11-12T02:00:00Z" : body.archived === false ? null : issue.archivedAt }; return { issue }; }
    if (options.method === "POST") { issue = { ...issue, ...body }; return { issue }; }
    if (path.includes("/issue-one")) return { issue };
    return { issues: [issue], total: 1 };
  });
});
afterEach(() => { cleanup(); vi.clearAllMocks(); });
function mount(route = "/w/one/work/mine") {
  function Route() { const { id } = useParams(); return <Issues key={id || "list"} workspaceId="one" userId="user" mine issueId={id} />; }
  const router = createMemoryRouter([{ path: "/w/one/work/mine/:id?", element: <Route /> }], { initialEntries: [route] });
  render(<RouterProvider router={router} />);
  return router;
}
test("My issues creates with the current assignee, opens a durable detail URL, and follows browser history", async () => {
  const user = userEvent.setup(); const router = mount();
  await screen.findByRole("link", { name: /First issue/ });
  expect(requests.some(request => request.path.includes("mine=true&archived=false"))).toBe(true);
  await user.click(screen.getByRole("button", { name: "New issue" }));
  await user.type(await screen.findByRole("textbox", { name: "Title" }), "A new issue");
  await user.click(screen.getByRole("button", { name: "Create issue" }));
  await screen.findByRole("heading", { name: "A new issue" });
  expect(router.state.location.pathname).toBe("/w/one/work/mine/issue-one");
  expect(requests.find(request => request.body?.title === "A new issue")?.body?.assigneeId).toBe("user");
  await act(async () => { await router.navigate(-1); });
  expect(router.state.location.pathname).toBe("/w/one/work/mine/new");
});
test("edit conflicts retain the draft until explicit reload; activity renders actor and field changes", async () => {
  const user = userEvent.setup(); mount("/w/one/work/mine/issue-one");
  await screen.findByRole("heading", { name: "First issue" });
  expect(screen.getByText("Morgan")).toBeTruthy();
  expect(screen.getByText("updated the issue")).toBeTruthy();
  expect(screen.getByText("Backlog")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Edit issue" }));
  await user.clear(screen.getByRole("textbox", { name: "Title" }));
  await user.type(screen.getByRole("textbox", { name: "Title" }), "Retained draft");
  conflict = true;
  await user.click(screen.getByRole("button", { name: "Save changes" }));
  await screen.findByRole("alert");
  expect((screen.getByRole("textbox", { name: "Title" }) as HTMLInputElement).value).toBe("Retained draft");
  expect(requests.find(request => request.body?.title === "Retained draft")?.body?.version).toBe(1);
  await user.click(screen.getByRole("button", { name: "Reload" }));
  await screen.findByRole("heading", { name: "First issue" });
  conflict = false;
  await user.click(screen.getByRole("button", { name: "Archive issue" }));
  await screen.findByRole("button", { name: "Restore issue" });
  await user.click(screen.getByRole("button", { name: "Restore issue" }));
  await screen.findByRole("button", { name: "Archive issue" });
});
test("archive filtering is recorded in the URL and survives detail navigation", async () => {
  const user = userEvent.setup(); const router = mount();
  await screen.findByRole("link", { name: /First issue/ });
  await user.click(screen.getByRole("button", { name: "Archived" }));
  await waitFor(() => expect(requests.some(request => request.path.includes("archived=true"))).toBe(true));
  expect(router.state.location.search).toBe("?archived=true");
  await user.click(await screen.findByRole("link", { name: /First issue/ }));
  await screen.findByRole("heading", { name: "First issue" });
  expect(router.state.location.search).toBe("?archived=true");
  await user.click(screen.getByRole("link", { name: "My issues" }));
  expect(router.state.location.search).toBe("?archived=true");
});
test("saving an unassigned issue keeps it unassigned and preserves the list page", async () => {
  issue.assigneeId = null;
  const user = userEvent.setup(); const router = mount("/w/one/work/mine/issue-one?offset=50&archived=true");
  await screen.findByRole("heading", { name: "First issue" });
  await user.click(screen.getByRole("button", { name: "Edit issue" }));
  await user.type(screen.getByRole("textbox", { name: "Title" }), " changed");
  await user.click(screen.getByRole("button", { name: "Save changes" }));
  await screen.findByRole("heading", { name: "First issue changed" });
  expect(requests.find(request => request.body?.title === "First issue changed")?.body?.assigneeId).toBe(null);
  await user.click(screen.getByRole("link", { name: "My issues" }));
  expect(router.state.location.search).toBe("?offset=50&archived=true");
});


test("attaching materials carries the current issue version and reloads without changing the human assignee", async () => {
  materials = [{ id: "hidden", assetKind: "sourceObject", assetId: null, displayName: "Secret name", available: false }];
  const user = userEvent.setup(); mount("/w/one/work/mine/issue-one");
  await screen.findByText("Material unavailable");
  expect(screen.queryByText("Secret name")).toBeNull();
  await user.click(screen.getByRole("button", { name: "Add materials" }));
  await user.click(await screen.findByRole("button", { name: "Attach" }));
  await waitFor(() => expect(issue.version).toBe(2));
  expect(requests.find(request => request.body?.assetId === "file")?.body).toEqual({ version: 1, assetKind: "userLibraryObject", assetId: "file" });
  await screen.findByRole("heading", { name: "First issue" });
  expect(issue.assigneeId).toBe("user");
});

test("execution results load on expansion and retain the full conversation and artifact links", async () => {
  executions = [{ id: "execution", sessionId: "session", title: "Research", status: "completed" }];
  const user = userEvent.setup(); mount("/w/one/work/mine/issue-one");
  await screen.findByRole("button", { name: "Research" });
  expect(requests.some(request => request.path.includes("/executions/execution?"))).toBe(false);
  await user.click(screen.getByRole("button", { name: "Research" }));
  await screen.findByText("Delivered analysis");
  expect(screen.getByText("Read the brief")).toBeTruthy();
  expect(screen.getByRole("link", { name: "Report.pdf" }).getAttribute("href")).toBe("/report");
  expect(screen.getByRole("link", { name: /Open conversation/ }).getAttribute("href")).toContain("session");
});
