// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { AgentChatPageContent } from "../../src/routes/AgentChatRoute";
import { ApiError, apiJson } from "../../src/api";
import { streamWorkspacePresentation } from "../../src/chat/workspacePresentationTransport";
import type { PresentationFrame } from "../../src/chat/workspacePresentationProtocol";

vi.mock("../../src/i18n", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("../../src/api", async original => ({ ...await original<object>(), apiJson: vi.fn() }));
vi.mock("../../src/chat/workspacePresentationTransport", () => ({ streamWorkspacePresentation: vi.fn() }));
vi.mock("react-router", async original => ({
  ...await original<object>(),
  useRouteLoaderData: (id: string) => id === "authenticated" ? { user: { id: "user" } } : {
    workspace: { id: "workspace" }, agents: ["agent", "other"].map(id => ({ id, workspaceId: "workspace", name: id, status: "active" })),
  },
}));
vi.mock("../../src/shell/ShellPage", () => ({ ShellPage: ({ children }: { children: React.ReactNode }) => <div>{children}</div> }));
vi.mock("../../src/agent-chat/AgentInputComposer", () => ({ AgentInputComposer: () => null }));
vi.mock("../../src/agent-chat/AgentMessageList", () => ({ AgentMessageList: ({ messages }: { messages: { text: string }[] }) =>
  <div>{messages.map((item, index) => <p key={index}>{item.text}</p>)}</div> }));

type Subscription = Parameters<typeof streamWorkspacePresentation>[0];
const subscriptions: Subscription[] = [];
const request = vi.mocked(apiJson);
let historyReads = 0;
const frame = (kind: "viewRevision", historyRevision: string, workRevision = "0"): PresentationFrame => ({
  schema: "workspace.presentation.v1", workspaceId: "workspace", agentId: "agent", sessionId: null,
  kind, contextRevision: "0", historyRevision, workRevision,
});
const settle = async () => { await act(async () => { await Promise.resolve(); }); };
beforeEach(() => {
  vi.useFakeTimers(); vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  historyReads = 0;
  request.mockImplementation(async path => {
    const agentId = String(path).includes("/other/") ? "other" : "agent";
    historyReads++;
    return { schema: "agent.history.v1", agentId, sessionId: "coord", items: [], nextCursor: null, newestCursor: null, hasMore: false };
  });
  vi.mocked(streamWorkspacePresentation).mockImplementation(options => {
    subscriptions.push(options);
    options.onConnection("running");
    return new Promise((_resolve, reject) => { options.signal.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")), { once: true }); });
  });
});
afterEach(() => { cleanup(); subscriptions.length = 0; vi.restoreAllMocks(); vi.unstubAllGlobals(); vi.clearAllMocks(); vi.useRealTimers(); });

test("Agent history uses one subscription and reads only initial content or a changed history revision", async () => {
  render(<AgentChatPageContent agentId="agent" />); await settle();
  expect(subscriptions).toHaveLength(1); expect(subscriptions[0].sessionId).toBe(null);
  await act(async () => { await vi.advanceTimersByTimeAsync(60_000); });
  expect(historyReads).toBe(1);
  await act(async () => { await subscriptions[0].controller.acceptWithBackpressure({ cursor: null, item: frame("viewRevision", "1") }); });
  expect(historyReads).toBe(2);
  await act(async () => {
    await subscriptions[0].controller.acceptWithBackpressure({ cursor: null, item: frame("viewRevision", "1") });
    await subscriptions[0].controller.acceptWithBackpressure({ cursor: null, item: frame("viewRevision", "1", "1") });
  });
  expect(historyReads).toBe(2);
  expect(subscriptions).toHaveLength(1);
});

test("Agent revisions received during a slow history read collapse into one follow-up", async () => {
  let finish!: (value: unknown) => void;
  request.mockImplementationOnce(() => {
    historyReads++;
    return new Promise(resolve => { finish = resolve; });
  });
  render(<AgentChatPageContent agentId="agent" />); await settle();
  await act(async () => {
    await subscriptions[0].controller.acceptWithBackpressure({ cursor: null, item: frame("viewRevision", "1") });
    await subscriptions[0].controller.acceptWithBackpressure({ cursor: null, item: frame("viewRevision", "2") });
  });
  expect(historyReads).toBe(1);
  await act(async () => { finish({ schema: "agent.history.v1", agentId: "agent", sessionId: "coord", items: [], nextCursor: null, newestCursor: null, hasMore: false }); });
  await settle(); expect(historyReads).toBe(2);
});

test("Agent scope changes cancel the original subscription and discard its later frames", async () => {
  const view = render(<AgentChatPageContent agentId="agent" />); await settle();
  const old = subscriptions[0];
  view.rerender(<AgentChatPageContent agentId="other" />); await settle();
  expect(old.signal.aborted).toBe(true); expect(subscriptions).toHaveLength(2);
  expect(subscriptions[1].agentId).toBe("other");
  const before = historyReads;
  await act(async () => { await old.controller.acceptWithBackpressure({ cursor: null, item: frame("viewRevision", "50") }); });
  expect(historyReads).toBe(before);
  expect(screen.queryByRole("alert")).toBe(null);
});

test("an Agent newer page that does not advance its newest cursor stops instead of repeatedly fetching", async () => {
  request.mockImplementation(async path => {
    historyReads++;
    if (historyReads > 3) throw new Error("fixture request safety stop");
    return { schema: "agent.history.v1", agentId: "agent", sessionId: "coord", items: [],
      nextCursor: String(path).includes("afterCursor=") ? "different-page-cursor" : "tail",
      newestCursor: "tail", hasMore: String(path).includes("afterCursor=") };
  });
  render(<AgentChatPageContent agentId="agent" />); await settle();
  await act(async () => { await subscriptions[0].controller.acceptWithBackpressure({ cursor: null, item: frame("viewRevision", "1") }); });
  await settle();
  expect(historyReads).toBe(2);
  expect(screen.getByRole("alert")).toBeTruthy();
});

test("one retry recovers both a failed history read and its failed presentation connection", async () => {
  request.mockRejectedValueOnce(new Error("history offline"));
  vi.mocked(streamWorkspacePresentation).mockRejectedValueOnce(new Error("stream offline"));
  render(<AgentChatPageContent agentId="agent" />); await settle();
  expect(screen.getByRole("alert")).toBeTruthy();
  await act(async () => { screen.getByRole("button", { name: "agentChat.retry" }).click(); }); await settle();
  expect(request).toHaveBeenCalledTimes(2);
  expect(vi.mocked(streamWorkspacePresentation)).toHaveBeenCalledTimes(2);
  expect(screen.queryByRole("alert")).toBe(null);
});


test("explicit retry restores initially forbidden history even when the presentation revision remains zero", async () => {
  request.mockRejectedValueOnce(new ApiError("forbidden", 403));
  vi.mocked(streamWorkspacePresentation).mockRejectedValueOnce(new ApiError("forbidden", 403));
  render(<AgentChatPageContent agentId="agent" />); await settle();
  expect(request).toHaveBeenCalledTimes(1);
  expect(screen.getByRole("alert")).toBeTruthy();
  request.mockResolvedValue({ schema: "agent.history.v1", agentId: "agent", sessionId: "coord", items: [{
    cursor: "restored", kind: "message", message: { id: "restored-message", agentRunId: "run", turnId: "turn", toolCallId: "call",
      sourceSequence: 1, createdAtMs: 1, body: "Restored after permission changed", sessionRefs: [], fileRefs: [] },
  }], nextCursor: "restored", newestCursor: "restored", hasMore: false });
  await act(async () => { screen.getByRole("button", { name: "agentChat.retry" }).click(); }); await settle();
  await act(async () => { await subscriptions[0].controller.acceptWithBackpressure({ cursor: null, item: frame("viewRevision", "0") }); });
  expect(vi.mocked(streamWorkspacePresentation)).toHaveBeenCalledTimes(2);
  expect(request).toHaveBeenCalledTimes(2);
  expect(screen.getByText("Restored after permission changed")).toBeTruthy();
  expect(screen.queryByRole("alert")).toBe(null);
});


const historyPage = (index: number, hasMore: boolean) => ({
  schema: "agent.history.v1", agentId: "agent", sessionId: "coord", items: index ? [{
    cursor: `cursor-${index}`, kind: "message", message: { id: `message-${index}`, agentRunId: "run", turnId: `turn-${index}`, toolCallId: `call-${index}`,
      sourceSequence: index, createdAtMs: index, body: `History item ${index}`, sessionRefs: [], fileRefs: [] },
  }] : [], nextCursor: `cursor-${index}`, newestCursor: `cursor-${index}`, hasMore,
});

test("each history invalidation has a four-page budget and explicit continuation preserves its cursor", async () => {
  const cursors: (string | null)[] = [];
  request.mockImplementation(async path => {
    const cursor = new URL(String(path), "https://synthetic.invalid").searchParams.get("afterCursor");
    cursors.push(cursor);
    const index = cursor === null ? 0 : Number(cursor.slice("cursor-".length)) + 1;
    if (index > 6) throw new Error("fixture page budget guard");
    return historyPage(index, index > 0 && index < 6);
  });
  render(<AgentChatPageContent agentId="agent" />); await settle();
  await act(async () => { await subscriptions[0].controller.acceptWithBackpressure({ cursor: null, item: frame("viewRevision", "1") }); });
  await settle();
  expect(request).toHaveBeenCalledTimes(5);
  expect(cursors).toEqual([null, "cursor-0", "cursor-1", "cursor-2", "cursor-3"]);
  expect(screen.getByText("History item 4")).toBeTruthy();
  await act(async () => { screen.getByRole("button", { name: "agentChat.loadMore" }).click(); }); await settle();
  expect(request).toHaveBeenCalledTimes(7);
  expect(cursors.slice(-2)).toEqual(["cursor-4", "cursor-5"]);
  expect(screen.getByText("History item 1")).toBeTruthy();
  expect(screen.getByText("History item 6")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "agentChat.loadMore" })).toBe(null);
});

test("a tab hidden during history catch-up does not request another page until visible", async () => {
  let finish!: (value: unknown) => void;
  request.mockResolvedValueOnce(historyPage(0, false))
    .mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }))
    .mockResolvedValue(historyPage(2, false));
  render(<AgentChatPageContent agentId="agent" />); await settle();
  await act(async () => { await subscriptions[0].controller.acceptWithBackpressure({ cursor: null, item: frame("viewRevision", "1") }); });
  expect(request).toHaveBeenCalledTimes(2);
  const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
  await act(async () => { document.dispatchEvent(new Event("visibilitychange")); finish(historyPage(1, true)); }); await settle();
  expect(request).toHaveBeenCalledTimes(2);
  expect(screen.getByText("History item 1")).toBeTruthy();
  visibility.mockReturnValue("visible");
  await act(async () => { document.dispatchEvent(new Event("visibilitychange")); }); await settle();
  expect(request).toHaveBeenCalledTimes(3);
  expect(String(request.mock.calls[2][0])).toContain("afterCursor=cursor-1");
  expect(screen.getByText("History item 2")).toBeTruthy();
});
