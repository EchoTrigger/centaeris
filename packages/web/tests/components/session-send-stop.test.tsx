// @vitest-environment jsdom
import { webcrypto } from "node:crypto";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import WorkspaceChatLayout from "../../src/routes/WorkspaceHomeRoute";
import type { SessionSummary } from "../../src/router";
import { AppPageContent } from "../../src/routes/AppRoute";
import type { streamWorkspacePresentation } from "../../src/chat/workspacePresentationTransport";
import type { TranscriptPage } from "../../src/chat/transcriptContract";
import type { TranscriptViewStore } from "../../src/chat/transcriptViewStore";
import type { PendingTranscriptMessage } from "../../src/chat/transcriptTurns";

type Subscription = Parameters<typeof streamWorkspacePresentation>[0];
const fixture = vi.hoisted(() => ({
  request: vi.fn<(path: string, options?: RequestInit) => Promise<unknown>>(),
  subscriptions: [] as Subscription[],
  navigate: vi.fn(),
  selectedSession: null as SessionSummary | null,
  user: { id: "user" }, workspace: { id: "workspace", name: "Synthetic workspace", avatarUrl: null as string | null },
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
  useRouteLoaderData: (id: string) => id === "authenticated" ? { user: fixture.user } : { workspace: fixture.workspace, selectedSession: fixture.selectedSession },
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
  TranscriptBlockList: ({ store, pendingUserMessage, running }: { store: TranscriptViewStore; pendingUserMessage: PendingTranscriptMessage | null; running: boolean }) =>
    <section aria-label="Synthetic transcript" data-running={String(running)}>
      {store.getListSnapshot().blockIds.map(id => { const block = store.getBlockSnapshot(id); return block?.body.kind === "userText" ? <p key={id}>{block.body.content.inlineContent}</p> : null; })}
      {pendingUserMessage ? <p data-submission-status={pendingUserMessage.status ?? "pending"} data-completed-at={pendingUserMessage.completedAtMs}>
        {pendingUserMessage.text}
      </p> : null}
    </section>,
}));
vi.mock("../../src/shell/ShellSidebar", () => ({ ShellSidebar: ({ sessionProps }: { sessionProps: { sessions: { id: string }[]; renderSessionRow: (session: { id: string }) => React.ReactNode } }) => <nav aria-label="Session navigation">{sessionProps.sessions.map(sessionProps.renderSessionRow)}</nav> }));
vi.mock("../../src/shell/HomePlane", () => ({ HomePlane: () => null, HomeQuickActions: () => null }));
vi.mock("../../src/components/WorkspaceContextPanel", () => ({ WorkspaceContextPanel: () => null }));
vi.mock("../../src/components/DocumentPreview", () => ({ DocumentPreview: () => null }));

const row = { id: "session", workspaceId: "workspace", projectId: null, currentEnvironmentId: null,
  executionMode: null, title: "Synthetic", origin: "user",  status: "active", deletedAt: null,
  isPinned: false, isUnread: false, hasActiveAgentRun: false, updatedAt: "2026-10-23T00:00:00Z",
  latestAttentionSequence: "0", readThroughSequence: "0", attentionRevision: "0", isManuallyUnread: false };
const page: TranscriptPage = { schema: "transcript.page.v1", sessionId: "session", projectionVersion: "transcript.projection.v1",
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
async function sendCheckpoint(status: "failed" | "cancelled" | null, sourceHighWater = "0") {
  const subscription = fixture.subscriptions.findLast(item => item.sessionId === "session" && !item.signal.aborted)!;
  await act(async () => {
    await subscription.controller.acceptWithBackpressure({ cursor: "applied:0", item: {
      schema: "workspace.presentation.v1", workspaceId: "workspace",  sessionId: "session", kind: "checkpoint",
      projectionGeneration: "generation", sourceHighWater, agentRun: status ? {
        agentRunId: "original-run", status, startedAt: null, completedAt: "2026-10-23T00:00:01Z",
      } : null,
    } });
  });
}
async function mountConversation(initialSession: typeof row | null = null, snapshot: TranscriptPage = page, workspaceLayout = false) {
  fixture.selectedSession = initialSession;
  render(<MemoryRouter initialEntries={[window.location.pathname + window.location.search]}>{workspaceLayout ? <WorkspaceChatLayout /> : <AppPageContent initialSession={initialSession} workspaceDraft={false} location={{ search: "?sessionId=session", state: null }} modelsVersion={0} onSessionAccepted={() => {}} />}</MemoryRouter>);
  await waitFor(() => expect(fixture.subscriptions.some(item => item.sessionId === "session" && !item.signal.aborted)).toBe(true));
  const subscription = fixture.subscriptions.findLast(item => item.sessionId === "session" && !item.signal.aborted)!;
  await act(async () => {
    await subscription.controller.acceptWithBackpressure({ cursor: null, item: {
      schema: "workspace.presentation.v1", workspaceId: "workspace",  sessionId: "session", kind: "snapshot", page: snapshot,
    } });
  });
  await sendCheckpoint(null, snapshot.sourceHighWater);
  await chooseModel();
}

async function chooseModel() {
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "appRoute.aiModel" }));
  await user.click(await screen.findByRole("menuitemradio", { name: "Synthetic model" }));
}

beforeEach(() => {
  sessionStorage.clear(); localStorage.clear(); fixture.activeRun = null; fixture.selectedSession = null;
  fixture.workspace = { id: "workspace", name: "Synthetic workspace", avatarUrl: null };
  vi.stubGlobal("crypto", webcrypto);
  window.history.replaceState({}, "", "/w/workspace/sessions/session?sessionId=session");
  fixture.request.mockImplementation(async path => {
    if (path === "/api/models?workspaceId=workspace") return { models: [{ id: "model", providerId: "provider", modelName: "model", displayName: "Synthetic model", thinkingModes: [] }] };
    if (path.includes("session-projects")) return { projects: [] };
    if (path.endsWith("/assets")) return { assets: [] };
    if (path === "/api/sessions/session") return { session: row };
    if (path === "/api/workspaces/workspace/sessions") return { sessions: [row] };
    if (path.endsWith("/context-usage")) return { schema: "session.context_usage.v1", sessionId: "session", contextUsage: null };
    throw new Error(`Unexpected synthetic request: ${path}`);
  });
});
afterEach(() => { cleanup(); fixture.subscriptions.length = 0; vi.clearAllMocks(); vi.unstubAllGlobals(); sessionStorage.clear(); localStorage.clear(); });

test("refreshing workspace profile retains the current Run subscription and conversation draft", async () => {
  fixture.activeRun = { agentRunId: "active-run", status: "running", streamCursor: "0" };
  const user = userEvent.setup();
  const props = { initialSession: row, workspaceDraft: false, location: { search: "?sessionId=session", state: null }, modelsVersion: 0, onSessionAccepted: () => {} };
  const view = render(<MemoryRouter><AppPageContent {...props} /></MemoryRouter>);
  await waitFor(() => expect(screen.queryByRole("button", { name: "appRoute.stop" })).not.toBeNull());
  await waitFor(() => expect(fixture.subscriptions.some(item => item.sessionId === "session" && !item.signal.aborted)).toBe(true));
  const current = fixture.subscriptions.findLast(item => item.sessionId === "session" && !item.signal.aborted)!;
  const count = fixture.subscriptions.length;
  await user.type(screen.getByRole("textbox", { name: "appRoute.message" }), "Keep this draft");
  fixture.workspace = { ...fixture.workspace, name: "Renamed workspace", avatarUrl: `/api/workspaces/workspace/avatar/${"a".repeat(64)}` };
  view.rerender(<MemoryRouter><AppPageContent {...props} /></MemoryRouter>);
  expect(fixture.subscriptions).toHaveLength(count);
  expect(current.signal.aborted).toBe(false);
  expect((screen.getByRole("textbox", { name: "appRoute.message" }) as HTMLTextAreaElement).value).toBe("Keep this draft");
  expect(screen.queryByRole("button", { name: "appRoute.stop" })).not.toBeNull();
});

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





test("a retained Session displays its original history and continues with the user selected model", async () => {
  const user = userEvent.setup(), navigation = deferred<unknown>();
  window.history.replaceState({}, "", "/w/workspace/sessions/session");
  const initial = fixture.request.getMockImplementation()!;
  fixture.request.mockImplementation((path, options) => path === "/api/workspaces/workspace/sessions" ? navigation.promise
    : path.endsWith("/messages") ? Promise.resolve(receiptFor(options)) : initial(path, options));
  await mountConversation(row, { ...page, sourceHighWater: "1", resumeCursors: [{ streamId: "workspace-transcript.v1", cursor: "1" }], blocks: [{
    blockId: "original-history", blockRevision: "1", orderKey: { sourceSequence: "1", ordinal: 0 },
    body: { kind: "userText", content: { inlineContent: "Original retained Session history", sourceRef: null } },
  }] }, true);
  expect(screen.getByText("Original retained Session history")).not.toBeNull();
  expect(screen.queryByRole("alert")).toBeNull();
  expect(fixture.subscriptions.every(item => item.sessionId === "session")).toBe(true);
  await act(async () => { navigation.resolve({ sessions: [row] }); });
  const historyLink = screen.getByRole("link", { name: "Synthetic" });
  expect(historyLink.getAttribute("href")).toBe("/w/workspace/sessions/session");
  await user.type(screen.getByRole("textbox", { name: "appRoute.message" }), "Continue this original Session");
  await user.click(screen.getByRole("button", { name: "appRoute.input" }));
  await waitFor(() => expect(callsFor("/messages")).toHaveLength(1));
  const [path, options] = callsFor("/messages")[0];
  const body = JSON.parse(String(options?.body));
  expect(path).toBe("/api/workspaces/workspace/sessions/session/messages");
  expect(body).not.toHaveProperty("agentId");
  expect(body.modelConfigRef).toBe("model");
  expect(body.thinkingMode).toBeUndefined();
  expect(screen.getByText("Original retained Session history")).not.toBeNull();
});

test("a new Session submits the model and thinking mode chosen by its user", async () => {
  vi.stubGlobal("matchMedia", () => ({ matches: true }));
  const user = userEvent.setup();
  const initial = fixture.request.getMockImplementation()!;
  fixture.request.mockImplementation((path, options) => path === "/api/models?workspaceId=workspace"
    ? Promise.resolve({ models: [
      { id: "model", providerId: "provider", modelName: "model", displayName: "Synthetic model", thinkingModes: [] },
      { id: "alternate", providerId: "provider", modelName: "alternate", displayName: "Alternate model", thinkingModes: ["low", "high"] },
    ] })
    : path.endsWith("/messages") ? Promise.resolve(receiptFor(options)) : initial(path, options));
  render(<MemoryRouter><AppPageContent workspaceDraft={false} location={{ search: "?new=1", state: null }} modelsVersion={0} onSessionAccepted={() => {}} /></MemoryRouter>);
  const modelPicker = screen.getByRole("button", { name: "appRoute.aiModel" });
  await waitFor(() => expect(modelPicker.textContent).toContain("appRoute.selectModel"));
  expect((modelPicker as HTMLButtonElement).disabled).toBe(false);
  await user.click(modelPicker);
  await user.click(await screen.findByRole("menuitemradio", { name: "Alternate model" }));
  expect(modelPicker.textContent).toContain("Alternate model");
  const thinkingPicker = screen.getByRole("button", { name: "appRoute.reasoningEffort" });
  expect((thinkingPicker as HTMLButtonElement).disabled).toBe(false);
  await user.click(thinkingPicker);
  await user.click(await screen.findByRole("menuitemradio", { name: "appRoute.high" }));
  await user.type(screen.getByRole("textbox", { name: "appRoute.message" }), "Use my selected model");
  await user.click(screen.getByRole("button", { name: "appRoute.input" }));
  await waitFor(() => expect(callsFor("/messages")).toHaveLength(1));
  const [path, options] = callsFor("/messages")[0];
  const body = JSON.parse(String(options?.body));
  expect(path).toBe("/api/workspaces/workspace/sessions/new/messages");
  expect(body).not.toHaveProperty("agentId");
  expect(body.modelConfigRef).toBe("alternate");
  expect(body.thinkingMode).toBe("high");
});

test("sending immediately after opening a Session targets that Session while navigation data is pending", async () => {
  vi.stubGlobal("matchMedia", () => ({ matches: true }));
  const user = userEvent.setup(), navigation = deferred<unknown>(), accepted = deferred<unknown>();
  window.history.replaceState({}, "", "/w/workspace/app");
  const initial = fixture.request.getMockImplementation()!;
  render(<MemoryRouter initialEntries={["/w/workspace/app"]}><WorkspaceChatLayout /></MemoryRouter>);
  await chooseModel();
  const oldSession = await screen.findByRole("link", { name: "Synthetic" });
  fixture.request.mockImplementation((path, options) => path === "/api/workspaces/workspace/sessions" ? navigation.promise
    : path.endsWith("/messages") ? accepted.promise : initial(path, options));
  fixture.selectedSession = row;
  await user.click(oldSession);
  await waitFor(() => expect(fixture.subscriptions.some(item => item.sessionId === "session" && !item.signal.aborted)).toBe(true));
  const subscription = fixture.subscriptions.findLast(item => item.sessionId === "session" && !item.signal.aborted)!;
  await act(async () => {
    await subscription.controller.acceptWithBackpressure({ cursor: null, item: {
      schema: "workspace.presentation.v1", workspaceId: "workspace",  sessionId: "session", kind: "snapshot", page,
    } });
  });
  await user.type(screen.getByRole("textbox", { name: "appRoute.message" }), "Continue the Session I just opened");
  await user.click(screen.getByRole("button", { name: "appRoute.input" }));
  await waitFor(() => expect(callsFor("/messages")).toHaveLength(1));
  expect(callsFor("/messages")[0][0]).toBe("/api/workspaces/workspace/sessions/session/messages");
});


test("restored questions from a later turn accept the original Run receipt and never leak across Session selection", async () => {
  const user = userEvent.setup();
  const originalRequest = fixture.request.getMockImplementation()!;
  fixture.request.mockImplementation(async (path, options) => {
    if (path.endsWith('/questions/question/responses')) return {
      operationId: JSON.parse(String(options?.body)).operationId, command: 'respondQuestion', status: 'accepted',
      sessionId: 'session', agentRunId: 'original-run', turnId: 'original-turn',
    };
    if (path === '/api/sessions/other') return { session: { ...row, id: 'other', title: 'Other Session' } };
    return originalRequest(path, options);
  });
  fixture.activeRun = { agentRunId: 'original-run', status: 'running', streamCursor: '0' };
  const props = { workspaceDraft: false, location: { search: '?sessionId=session', state: null }, modelsVersion: 0, onSessionAccepted: () => {} };
  const mount = () => render(<MemoryRouter><AppPageContent {...props} initialSession={row} /></MemoryRouter>);
  let view = mount();
  const restore = async () => {
    await waitFor(() => expect(screen.queryByRole('button', { name: 'appRoute.stop' })).not.toBeNull());
    const subscription = fixture.subscriptions.findLast(item => item.sessionId === 'session' && !item.signal.aborted)!;
    await act(async () => {
      await subscription.controller.acceptWithBackpressure({ cursor: null, item: { schema: 'workspace.presentation.v1', workspaceId: 'workspace', sessionId: 'session', kind: 'snapshot', page } });
      await subscription.controller.acceptWithBackpressure({ cursor: null, item: { schema: 'workspace.presentation.v1', workspaceId: 'workspace', sessionId: 'session', kind: 'checkpoint',
        projectionGeneration: 'generation', sourceHighWater: '0', agentRun: { agentRunId: 'original-run', status: 'running', startedAt: null, completedAt: null } } });
      await subscription.controller.acceptWithBackpressure({ cursor: null, item: { schema: 'workspace.presentation.v1', workspaceId: 'workspace', sessionId: 'session', kind: 'question',
        agentRunId: 'original-run', sourceHighWater: '0', question: { questionId: 'question', continuationId: 'continuation', turnId: 'later-question-turn', question: 'Choose delivery', options: ['Summary'] } } });
    });
    expect(screen.getByRole('region', { name: 'Choose delivery' })).toBeTruthy();
    return subscription;
  };
  const oldSubscription = await restore();
  view.unmount();
  view = mount();
  const currentSubscription = await restore();
  await user.click(screen.getByRole('button', { name: /1 Summary/ }));
  await user.click(screen.getByRole('button', { name: 'sessionQuestion.send' }));
  await waitFor(() => expect(screen.queryByRole('region', { name: 'Choose delivery' })).toBeNull());
  const responses = callsFor('/responses');
  expect(responses).toHaveLength(1);
  expect(responses[0][0]).toBe('/api/sessions/session/agent-runs/original-run/questions/question/responses');
  expect(JSON.parse(String(responses[0][1]?.body))).toMatchObject({ continuationId: 'continuation', action: 'answer', answer: '1. Summary' });
  expect(callsFor('/messages')).toHaveLength(0);
  await act(async () => currentSubscription.controller.acceptWithBackpressure({ cursor: null, item: {
    schema: 'workspace.presentation.v1', workspaceId: 'workspace', sessionId: 'session', kind: 'question', agentRunId: 'original-run', sourceHighWater: '0',
    question: { questionId: 'question', continuationId: 'next-continuation', turnId: 'next-question-turn', question: 'Choose next step', options: ['Check documents', 'Finish'] },
  } }));
  expect(screen.getByRole('region', { name: 'Choose next step' })).toBeTruthy();
  expect(screen.getByRole('button', { name: 'sessionQuestion.send' })).toHaveProperty('disabled', true);
  fixture.activeRun = null;
  view.rerender(<MemoryRouter><AppPageContent {...props} initialSession={{ ...row, id: 'other', title: 'Other Session' }} location={{ search: '?sessionId=other', state: null }} /></MemoryRouter>);
  await act(async () => oldSubscription.controller.acceptWithBackpressure({ cursor: null, item: {
    schema: 'workspace.presentation.v1', workspaceId: 'workspace', sessionId: 'session', kind: 'question', agentRunId: 'original-run', sourceHighWater: '0',
    question: { questionId: 'old-question', continuationId: 'continuation', turnId: 'original-turn', question: 'Stale question', options: [] },
  } }));
  expect(screen.queryByText('Stale question')).toBeNull();
  expect(screen.queryByText('Choose delivery')).toBeNull();
});
