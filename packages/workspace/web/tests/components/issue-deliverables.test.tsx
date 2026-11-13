// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router";
import type { ReactNode } from "react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { ApiError, apiJson } from "../../src/api";
import { DeliverableSubmit, IssueDeliverables } from "../../src/work/IssueDeliverables";

vi.mock("../../src/api", async importOriginal => ({ ...await importOriginal<typeof import("../../src/api")>(), apiJson: vi.fn() }));
vi.mock("../../src/i18n", () => ({ useTranslation: () => ({ t: (key: string, options?: { revision?: number }) => options?.revision ? `${key}:${options.revision}` : key }) }));

const issue = { id: "issue", version: 4, creatorId: "recipient", assigneeId: "author", visibility: "participants", archivedAt: null };
const run = { id: "run", summary: "Prepared report", artifacts: [{ id: "report", displayName: "Report.pdf", downloadUrl: "/private/report" }, { id: "notes", displayName: "Notes.txt", downloadUrl: "/private/notes" }] };
const current = { id: "current", issueId: "issue", executionId: "execution", agentRunId: "run", author: { id: "author", name: "Taylor" }, recipient: { id: "recipient", name: "Morgan" }, revision: 2, replacesId: "previous", summary: "Submitted report", artifacts: [{ id: "report", displayName: "Report.pdf", downloadUrl: "/shared/report" }], responses: [], createdAt: "2026-11-13T05:00:00Z", isCurrent: true };
let rows: unknown[];
let conflict: boolean;
let posts: { path: string; body: Record<string, unknown> }[];
beforeEach(() => {
  rows = [current]; conflict = false; posts = [];
  vi.mocked(apiJson).mockImplementation(async (path, options = {}) => {
    if (options.method === "POST") {
      const body = JSON.parse(String(options.body)); posts.push({ path, body });
      if (conflict) throw new ApiError("issue_version_conflict", 409);
      return { version: 5, deliverable: current };
    }
    if (path.endsWith("/assignees")) return { assignees: [{ id: "author", name: "Taylor" }, { id: "recipient", name: "Morgan" }, { id: "unrelated", name: "Outside participant" }] };
    return { version: 4, deliverables: rows };
  });
});
afterEach(() => { cleanup(); vi.clearAllMocks(); });
function mount(ui: ReactNode, path = "/") { return render(<MemoryRouter initialEntries={[path]}>{ui}</MemoryRouter>); }

test("submission shares only explicitly selected files with its selected recipient and revision", async () => {
  const user = userEvent.setup(); const onChanged = vi.fn(); const onCancel = vi.fn();
  mount(<DeliverableSubmit workspaceId="workspace" userId="author" issue={issue} executionId="execution" run={run} onChanged={onChanged} onCancel={onCancel} />);
  const submit = screen.getByRole("button", { name: "deliverables.submit" });
  await waitFor(() => expect((submit as HTMLButtonElement).disabled).toBe(false));
  expect(posts).toHaveLength(0);
  expect((screen.getByRole("checkbox", { name: "Report.pdf" }) as HTMLInputElement).checked).toBe(false);
  expect((screen.getByRole("checkbox", { name: "Notes.txt" }) as HTMLInputElement).checked).toBe(false);
  expect(screen.queryByRole("option", { name: "Outside participant" })).toBeNull();
  await user.click(screen.getByRole("checkbox", { name: "Report.pdf" }));
  await user.selectOptions(screen.getByRole("combobox", { name: "deliverables.replaces" }), "current");
  await user.click(submit);
  await waitFor(() => expect(onChanged).toHaveBeenCalledOnce());
  expect(onCancel).toHaveBeenCalledOnce();
  expect(posts[0]).toEqual({ path: "/api/workspaces/workspace/issues/issue/deliverables", body: { version: 4, executionId: "execution", agentRunId: "run", summary: "Prepared report", artifactIds: ["report"], recipientId: "recipient", replacesId: "current" } });
});

test("a rejected submission retains its edited draft and selected files until explicit reload", async () => {
  conflict = true;
  const user = userEvent.setup(); const onChanged = vi.fn();
  mount(<DeliverableSubmit workspaceId="workspace" userId="author" issue={issue} executionId="execution" run={run} onChanged={onChanged} onCancel={vi.fn()} />);
  const summary = screen.getByRole("textbox", { name: "deliverables.summary" });
  await user.clear(summary); await user.type(summary, "Keep my edited report");
  await user.click(screen.getByRole("checkbox", { name: "Notes.txt" }));
  await waitFor(() => expect((screen.getByRole("button", { name: "deliverables.submit" }) as HTMLButtonElement).disabled).toBe(false));
  await user.click(screen.getByRole("button", { name: "deliverables.submit" }));
  await screen.findByRole("alert");
  expect((summary as HTMLTextAreaElement).value).toBe("Keep my edited report");
  expect((screen.getByRole("checkbox", { name: "Notes.txt" }) as HTMLInputElement).checked).toBe(true);
  expect(onChanged).not.toHaveBeenCalled();
  await user.click(screen.getByRole("button", { name: "issues.reload" }));
  expect(onChanged).toHaveBeenCalledOnce();
});

test("authors can send feedback but cannot accept their recipient's work", async () => {
  const user = userEvent.setup(); const onChanged = vi.fn();
  mount(<IssueDeliverables workspaceId="workspace" userId="author" issue={issue} onChanged={onChanged} />);
  await screen.findByText("Submitted report");
  expect(screen.getByRole("link", { name: "Report.pdf" }).getAttribute("href")).toBe("/shared/report");
  expect(screen.queryByRole("button", { name: "deliverables.accept" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "deliverables.giveFeedback" }));
  await user.type(screen.getByRole("textbox", { name: "deliverables.feedback" }), "Please include the source clause");
  await user.click(screen.getByRole("button", { name: "deliverables.sendFeedback" }));
  await waitFor(() => expect(onChanged).toHaveBeenCalledOnce());
  expect(posts[0]).toEqual({ path: "/api/workspaces/workspace/issues/issue/deliverables/current/responses", body: { version: 4, action: "feedback", text: "Please include the source clause" } });
});

test("only the current recipient can accept and superseded revisions have no response actions", async () => {
  rows = [{ ...current, id: "previous", revision: 1, isCurrent: false }, current];
  const user = userEvent.setup(); const onChanged = vi.fn();
  mount(<IssueDeliverables workspaceId="workspace" userId="recipient" issue={issue} onChanged={onChanged} />);
  await screen.findAllByText("Submitted report");
  expect(screen.getAllByRole("button", { name: "deliverables.giveFeedback" })).toHaveLength(1);
  await user.click(screen.getByRole("button", { name: "deliverables.accept" }));
  await waitFor(() => expect(onChanged).toHaveBeenCalledOnce());
  expect(posts[0].body).toEqual({ version: 4, action: "accept", text: "" });
});

test("an empty submission list introduces no placeholder text or controls", async () => {
  rows = [];
  const { container } = mount(<IssueDeliverables workspaceId="workspace" userId="author" issue={issue} onChanged={vi.fn()} />);
  await waitFor(() => expect(apiJson).toHaveBeenCalled());
  expect(container.textContent).toBe("");
});

test("a new authorization denial clears previously loaded submission content", async () => {
  const { rerender } = mount(<IssueDeliverables workspaceId="workspace" userId="author" issue={issue} onChanged={vi.fn()} />);
  await screen.findByText("Submitted report");
  vi.mocked(apiJson).mockRejectedValue(new ApiError("issue_not_found", 404));
  rerender(<MemoryRouter><IssueDeliverables workspaceId="workspace" userId="author" issue={{ ...issue, version: 5 }} onChanged={vi.fn()} /></MemoryRouter>);
  await screen.findByRole("alert");
  expect(screen.queryByText("Submitted report")).toBeNull();
  expect(screen.queryByRole("link", { name: "Report.pdf" })).toBeNull();
});
