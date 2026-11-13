// @vitest-environment jsdom
import { webcrypto } from "node:crypto";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { apiJson } from "../../src/api";
import { IssueExecutions } from "../../src/work/IssueExecutions";
import { i18n } from "../../src/i18n";

vi.mock("../../src/api", async original => ({ ...await original<typeof import("../../src/api")>(), apiJson: vi.fn() }));
beforeEach(async () => { await i18n.changeLanguage("en"); sessionStorage.clear(); localStorage.clear(); vi.stubGlobal("crypto", webcrypto); });
afterEach(() => { cleanup(); vi.clearAllMocks(); vi.unstubAllGlobals(); });
const issue = { id: "issue", version: 3, archivedAt: null };
function mount() {
  const router = createMemoryRouter([{ path: "/issue", element: <IssueExecutions workspaceId="one" userId="user" issue={issue} /> }, { path: "/w/one/sessions/:id", element: <p>Conversation opened</p> }], { initialEntries: ["/issue"] });
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
  await user.click(screen.getByRole("button", { name: "Run with Agent" }));
  const select = await screen.findByRole("combobox", { name: "Model" });
  await waitFor(() => expect((select as HTMLSelectElement).disabled).toBe(false));
  await user.selectOptions(select, "model");
  await user.click(screen.getByRole("button", { name: "Start" }));
  await screen.findByText("Conversation opened");
  expect(submitted).toMatchObject({ version: 3, modelConfigRef: "model", thinkingMode: "low" });
  expect(typeof submitted?.operationId).toBe("string");
  expect(router.state.location.pathname).toBe("/w/one/sessions/session");
});
test("existing execution links open the same persisted conversation", async () => {
  vi.mocked(apiJson).mockResolvedValue({ executions: [{ id: "execution", sessionId: "existing", agentRunId: "run", title: "审核资料", status: "completed", createdAt: "2026-11-13T01:00:00Z" }], total: 1 });
  const router = mount(); const user = userEvent.setup();
  await user.click(await screen.findByRole("link", { name: /审核资料/ }));
  await screen.findByText("Conversation opened");
  expect(router.state.location.pathname).toBe("/w/one/sessions/existing");
});
