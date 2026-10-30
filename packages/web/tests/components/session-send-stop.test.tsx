// @vitest-environment jsdom
import { webcrypto } from "node:crypto";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { AppPageContent } from "../../src/routes/AppRoute";
import type { streamWorkspacePresentation } from "../../src/chat/workspacePresentationTransport";
import type { PendingTranscriptMessage } from "../../src/chat/transcriptTurns";

type Subscription = Parameters<typeof streamWorkspacePresentation>[0];
const fixture = vi.hoisted(() => ({
  request: vi.fn<(path: string, options?: RequestInit) => Promise<unknown>>(),
  subscriptions: [] as Subscription[],
  navigate: vi.fn(),
  user: { id: "user" }, workspace: { id: "workspace", name: "Synthetic workspace" },
  agents: [{ id: "agent", workspaceId: "workspace", name: "Synthetic agent", status: "active" }],
  activeRun: null as { agentRunId: string; status: string; streamCursor: string } | null,
}));

vi.mock("../../src/api", async original => ({
  ...await original<object>(),
  apiJson: (path: string, options?: RequestInit) => fixture.request(path, options),
  apiResponse: async (path: string, options?: RequestInit) => {
    const body = await fixture.request(path, options);
    return { json: async () => body };
  },
}));
vi.mock("react-router", async original => ({
  ...await original<object>(),
  useNavigate: () => fixture.navigate,
  useRouteLoaderData: (id: string) => id === "authenticated" ? { user: fixture.user } : { workspace: fixture.workspace, agents: fixture.agents },
}));
vi.mock("../../src/i18n", () => {
  const t = (key: string) => key;
  return { t, i18n: { language: "en" }, useTranslation: () => ({ t }) };
});
vi.mock("../../src/preferences", () => ({
  useEnterStartsNewLine: () => false, readModelThinkingMode: () => "", readPreferredModelIdentity: () => "",
  writeModelThinkingMode: () => {}, writePreferredModelIdentity: () => {},
}));
vi.mock("../../src/chat/transcriptTransport", () => ({
  createWorkspaceTranscriptTransport: () => ({ loadActiveAgentRun: async () => ({ agentRun: fixture.activeRun }) }),
}));
vi.mock("../../src/chat/workspacePresentationTransport", () => ({
  streamWorkspacePresentation: (options: Subscription) => {
    fixture.subscriptions.push(options);
    options.onConnection("running");
    return new Promise<void>(resolve => options.signal.addEventListener("abort", () => resolve(), { once: true }));
  },
}));
vi.mock("../../src/chat/TranscriptBlockList", () => ({
  TranscriptBlockList: ({ pendingUserMessage, running }: { pendingUserMessage: PendingTranscriptMessage | null; running: boolean }) =>
    <section aria-label="Synthetic transcript" data-running={String(running)}>
      {pendingUserMessage ? <p data-submission-status={pendingUserMessage.status ?? "pending"} data-completed-at={pendingUserMessage.completedAtMs}>
        {pendingUserMessage.text}
      </p> : null}
    </section>,
}));
vi.mock("../../src/shell/ShellSidebar", () => ({ ShellSidebar: () => null }));
vi.mock("../../src/shell/HomePlane", () => ({ HomePlane: () => null, HomeQuickActions: () => null }));
vi.mock("../../src/shell/useAgentDocumentIdentity", () => ({ AgentDocumentIdentity: () => null }));
vi.mock("../../src/agent-chat/SessionAgentBreadcrumb", () => ({ SessionAgentBreadcrumb: () => null }));
vi.mock("../../src/components/WorkspaceContextPanel", () => ({ WorkspaceContextPanel: () => null }));
vi.mock("../../src/components/DocumentPreview", () => ({ DocumentPreview: () => null }));

const row = { id: "session", workspaceId: "workspace", agentId: "agent", projectId: null, currentEnvironmentId: null,
  executionMode: null, title: "Synthetic", origin: "user", initialInputOrigin: null, status: "active", deletedAt: null,
  isPinned: false, isUnread: false, hasActiveAgentRun: false, updatedAt: "2026-10-23T00:00:00Z",
  latestAttentionSequence: "0", readThroughSequence: "0", attentionRevision: "0", isManuallyUnread: false };
const page = { schema: "transcript.page.v1", sessionId: "session", projectionVersion: "transcript.projection.v1",
  projectionGeneration: "generation", sourceHighWater: "0", blocks: [], olderCursor: null, hasOlder: false,
  resumeCursors: [] };
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}
const callsFor = (suffix: string) => fixture.request.mock.calls.filter(([path, options]) => path.endsWith(suffix) && options?.method === "POST");
const receiptFor = (options?: RequestInit) => ({ operationId: JSON.parse(String(options?.body)).operationId,
  command: "submitMessage", status: "accepted", sessionId: "session", agentRunId: "original-run", turnId: "original-turn" });
async function sendCheckpoint(status: "failed" | "cancelled" | null) {
  const subscription = fixture.subscriptions.findLast(item => item.sessionId === "session" && !item.signal.aborted)!;
  await act(async () => {
    await subscription.controller.acceptWithBackpressure({ cursor: "applied:0", item: {
      schema: "workspace.presentation.v1", workspaceId: "workspace", agentId: "agent", sessionId: "session", kind: "checkpoint",
      projectionGeneration: "generation", sourceHighWater: "0", agentRun: status ? {
        agentRunId: "original-run", status, startedAt: null, completedAt: "2026-10-23T00:00:01Z",
      } : null,
    } });
  });
}
async function mountConversation() {
  render(<AppPageContent agentId="agent" workspaceDraft={false} location={{ search: "?sessionId=session", state: null }} modelsVersion={0} onSessionAccepted={() => {}} />);
  await waitFor(() => expect(fixture.subscriptions.some(item => item.sessionId === "session" && !item.signal.aborted)).toBe(true));
  const subscription = fixture.subscriptions.findLast(item => item.sessionId === "session" && !item.signal.aborted)!;
  await act(async () => {
    await subscription.controller.acceptWithBackpressure({ cursor: null, item: {
      schema: "workspace.presentation.v1", workspaceId: "workspace", agentId: "agent", sessionId: "session", kind: "snapshot", page,
    } });
  });
  await sendCheckpoint(null);
  await waitFor(() => expect(screen.getByRole("button", { name: "appRoute.aiModel" }).textContent).toContain("Synthetic model"));
}

beforeEach(() => {
  sessionStorage.clear(); localStorage.clear(); fixture.activeRun = null;
  vi.stubGlobal("crypto", webcrypto);
  window.history.replaceState({}, "", "/w/workspace/agents/agent?sessionId=session");
  fixture.request.mockImplementation(async path => {
    if (path === "/api/models?workspaceId=workspace") return { models: [{ id: "model", providerId: "provider", modelName: "model", displayName: "Synthetic model", thinkingModes: [] }] };
    if (path.includes("session-projects")) return { projects: [] };
    if (path.endsWith("/assets")) return { assets: [] };
    if (path === "/api/sessions/session") return { session: row };
    if (path === "/api/workspaces/workspace/sessions?agentId=agent") return { sessions: [row] };
    if (path.endsWith("/context-usage")) return { schema: "session.context_usage.v1", sessionId: "session", contextUsage: null };
    throw new Error(`Unexpected synthetic request: ${path}`);
  });
});
afterEach(() => { cleanup(); fixture.subscriptions.length = 0; vi.clearAllMocks(); vi.unstubAllGlobals(); sessionStorage.clear(); localStorage.clear(); });

test("Send immediately exposes Stop and a deferred receipt cancels only its original Run", async () => {
  const user = userEvent.setup(), accepted = deferred<unknown>();
  const initial = fixture.request.getMockImplementation()!;
  fixture.request.mockImplementation((path, options) => path.endsWith("/messages") ? accepted.promise
    : path.endsWith("/cancel") ? Promise.resolve({ agentRunId: "original-run", status: "queued", disposition: "requested" }) : initial(path, options));
  await mountConversation();
  await user.type(screen.getByRole("textbox", { name: "appRoute.message" }), "Synthetic original input");
  await user.click(screen.getByRole("button", { name: "appRoute.input" }));
  const stop = screen.getByRole("button", { name: "appRoute.stop" });
  expect((stop as HTMLButtonElement).disabled).toBe(false);
  await waitFor(() => expect(callsFor("/messages")).toHaveLength(1));
  await user.click(stop);
  expect(callsFor("/cancel")).toHaveLength(0);
  expect(screen.queryByRole("status")).toBeNull();
  await act(async () => { accepted.resolve(receiptFor(callsFor("/messages")[0][1])); });
  await waitFor(() => expect(callsFor("/cancel")).toHaveLength(1));
  expect(callsFor("/cancel")[0][0]).toBe("/api/sessions/session/agent-runs/original-run/cancel");
  expect(callsFor("/messages")).toHaveLength(1);
  expect(screen.queryByRole("status")).toBeNull();
});

test("repeated mouse and Space activation during cancellation keeps one request and the original compact controls", async () => {
  const user = userEvent.setup(), cancellation = deferred<unknown>();
  const initial = fixture.request.getMockImplementation()!;
  fixture.request.mockImplementation((path, options) => path.endsWith("/messages") ? Promise.resolve(receiptFor(options))
    : path.endsWith("/cancel") ? cancellation.promise : initial(path, options));
  await mountConversation();
  await user.type(screen.getByRole("textbox", { name: "appRoute.message" }), "Synthetic original input");
  await user.click(screen.getByRole("button", { name: "appRoute.input" }));
  await waitFor(() => expect(screen.queryByRole("button", { name: "appRoute.stop" })).not.toBeNull());
  const stop = screen.getByRole("button", { name: "appRoute.stop" });
  await user.click(stop);
  await waitFor(() => expect(callsFor("/cancel")).toHaveLength(1));
  await user.click(stop); stop.focus(); await user.keyboard(" ");
  expect(callsFor("/cancel")).toHaveLength(1);
  expect((stop as HTMLButtonElement).disabled).toBe(false);
  expect(stop.getAttribute("aria-busy")).toBe("true");
  expect(screen.queryByRole("status")).toBeNull();
  await act(async () => { cancellation.resolve({ agentRunId: "original-run", status: "queued", disposition: "requested" }); });
  expect(callsFor("/messages")).toHaveLength(1);
  expect(screen.queryByRole("status")).toBeNull();
});

test("a failed checkpoint without a Core message ends the pending submission while retaining its input", async () => {
  const user = userEvent.setup();
  const initial = fixture.request.getMockImplementation()!;
  fixture.request.mockImplementation((path, options) => path.endsWith("/messages") ? Promise.resolve(receiptFor(options)) : initial(path, options));
  await mountConversation();
  await user.type(screen.getByRole("textbox", { name: "appRoute.message" }), "Synthetic original input");
  await user.click(screen.getByRole("button", { name: "appRoute.input" }));
  await waitFor(() => expect(screen.queryByRole("button", { name: "appRoute.stop" })).not.toBeNull());
  await sendCheckpoint("failed");
  const transcript = screen.getByRole("region", { name: "Synthetic transcript" });
  await waitFor(() => expect(transcript.getAttribute("data-running")).toBe("false"));
  const pending = screen.getByText("Synthetic original input");
  expect(pending.getAttribute("data-submission-status")).toBe("failed");
  expect(pending.getAttribute("data-completed-at")).toBe(String(Date.parse("2026-10-23T00:00:01Z")));
  expect(callsFor("/messages")).toHaveLength(1);
  expect(callsFor("/cancel")).toHaveLength(0);
});
