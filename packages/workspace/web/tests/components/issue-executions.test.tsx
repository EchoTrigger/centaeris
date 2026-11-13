// @vitest-environment jsdom
import { webcrypto } from "node:crypto";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { ApiError, apiJson } from "../../src/api";
import { IssueExecutions } from "../../src/work/IssueExecutions";
import { IssueConversationLink } from "../../src/work/IssueConversationLink";
import { i18n } from "../../src/i18n";

vi.mock("../../src/api", async original => ({ ...await original<typeof import("../../src/api")>(), apiJson: vi.fn() }));
beforeEach(async () => { await i18n.changeLanguage("en"); sessionStorage.clear(); localStorage.clear(); vi.stubGlobal("crypto", webcrypto); });
afterEach(() => { cleanup(); vi.clearAllMocks(); vi.unstubAllGlobals(); });
const issue = { id: "issue", version: 3, archivedAt: null };
function mount(path = "/issue") {
  const router = createMemoryRouter([{ path: "/issue", element: <IssueExecutions workspaceId="one" userId="user" issue={issue} /> }, { path: "/w/one/sessions/:id", element: <p>Conversation opened</p> }], { initialEntries: [path] });
  render(<RouterProvider router={router} />); return router;
}
test("Issue starts a real operation with selected model and opens the accepted conversation", async () => {
  let submitted: Record<string, unknown> | undefined;
  vi.mocked(apiJson).mockImplementation(async (path, options = {}) => {
    if (path.includes("/executions?")) return { executions: [], total: 0 };
    if (path.endsWith("/models")) return { models: [{ id: "model", modelName: "Fast", providerId: "provider", enabled: true, thinkingMode: "low", thinkingModes: ["low", "high"] }] };
    if (options.method === "POST") { submitted = JSON.parse(String(options.body)); return { operationId: submitted!.operationId, command: "submitMessage", status: "accepted", sessionId: "session", agentRunId: "run", turnId: "turn" }; }
    if (path === "/api/sessions/session") return { session: { id: "session", workspaceId: "one" } };
    throw new Error(path);
  });
  const router = mount(); const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Delegate to Agent" }));
  await user.type(screen.getByRole("textbox", { name: "Instructions (optional)" }), "Inspect the public API");
  const select = await screen.findByRole("combobox", { name: "Model" });
  await waitFor(() => expect((select as HTMLSelectElement).disabled).toBe(false));
  await user.selectOptions(select, "model");
  await user.click(screen.getByRole("button", { name: "Start" }));
  await screen.findByText("Conversation opened");
  expect(submitted).toMatchObject({ version: 3, modelConfigRef: "model", thinkingMode: "low", instructions: "Inspect the public API" });
  expect(typeof submitted?.operationId).toBe("string");
  expect(router.state.location.pathname).toBe("/w/one/sessions/session");
});
test("existing execution links open the same persisted conversation", async () => {
  vi.mocked(apiJson).mockResolvedValue({ executions: [{ id: "execution", sessionId: "existing", agentRunId: "run", title: "审核资料", status: "completed", createdAt: "2026-11-13T01:00:00Z" }], total: 1 });
  const router = mount(); const user = userEvent.setup();
  await user.click(await screen.findByRole("link", { name: "Continue conversation" }));
  await screen.findByText("Conversation opened");
  expect(router.state.location.pathname).toBe("/w/one/sessions/existing");
});

test("a returned Issue opens the selected execution's latest results without submitting a deliverable", async () => {
  vi.mocked(apiJson).mockImplementation(async path => {
    if (path.includes("/executions?")) return { executions: [{ id: "execution", sessionId: "existing", title: "Check requirements", status: "completed", latestAgentRunId: "followup", waitingForInput: false }], total: 1 };
    if (path.includes("/executions/execution?")) return { runs: [{ id: "followup", status: "completed", summary: "Revised analysis", artifacts: [], activity: [] }], total: 1 };
    throw new Error(path);
  });
  mount("/issue?execution=execution");
  await screen.findByText("Revised analysis");
  expect(screen.queryByText("Completed", { exact: true })).toBeNull();
  expect(screen.queryByRole("textbox", { name: /Summary/ })).toBeNull();
  expect(vi.mocked(apiJson).mock.calls.every(([, options]) => !options?.method)).toBe(true);
});

test("waiting sessions send the user to the existing question and distinguish a new conversation", async () => {
  vi.mocked(apiJson).mockResolvedValue({ executions: [{ id: "execution", sessionId: "waiting", title: "Check requirements", status: "running", waitingForInput: true }], total: 1 });
  const router = mount(); const user = userEvent.setup();
  const answer = await screen.findByRole("link", { name: /Answer Agent.*Waiting for input/ });
  expect(screen.getByRole("button", { name: "New Agent conversation" })).toBeTruthy();
  await user.click(answer);
  expect(router.state.location.pathname).toBe("/w/one/sessions/waiting");
});

test("conversation origins restore the linked Issue and selected execution after a refresh", async () => {
  vi.mocked(apiJson).mockResolvedValue({ origin: { issueId: "linked", executionId: "execution", title: "Check requirements" } });
  const router = createMemoryRouter([{ path: "/chat", element: <IssueConversationLink workspaceId="one" sessionId="session" /> }, { path: "/w/one/work/tasks/linked", element: <p>Issue opened</p> }], { initialEntries: ["/chat"] });
  render(<RouterProvider router={router} />);
  await userEvent.setup().click(await screen.findByRole("link", { name: "Back to issue" }));
  await screen.findByText("Issue opened");
  expect(router.state.location.search).toBe("?execution=execution");
});

test("an unrelated conversation or denied origin never retains the previous Issue link", async () => {
  vi.mocked(apiJson).mockResolvedValue({ origin: { issueId: "linked", executionId: "execution", title: "Check requirements" } });
  const { rerender } = render(<RouterProvider router={createMemoryRouter([{ path: "/", element: <IssueConversationLink workspaceId="one" sessionId="session" /> }])} />);
  await screen.findByRole("link", { name: "Back to issue" });
  vi.mocked(apiJson).mockRejectedValue(new ApiError("session_not_found", 404));
  rerender(<RouterProvider router={createMemoryRouter([{ path: "/", element: <IssueConversationLink workspaceId="one" sessionId="other" /> }])} />);
  await screen.findByRole("button", { name: "Retry issue link" });
  expect(screen.queryByRole("link", { name: "Back to issue" })).toBeNull();
  vi.mocked(apiJson).mockResolvedValue({ origin: null });
  await userEvent.setup().click(screen.getByRole("button", { name: "Retry issue link" }));
  await waitFor(() => expect(screen.queryByRole("button", { name: "Retry issue link" })).toBeNull());
});
