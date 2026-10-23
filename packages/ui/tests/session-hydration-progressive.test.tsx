// @vitest-environment jsdom
import { useRef, useState } from "react";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { getAgentContextUsage, getAgentRuntimeConfig, getAgentState, listAgentRuns, type AgentContextUsageSummary, type AgentRuntimeConfig } from "../src/lib/chatBridge";
import { synchronizeSessionTranscript } from "../src/components/chat/sessionTranscriptSync";
import { buildSessionHydrationSnapshot, sessionViewCacheStore } from "../src/components/chat/chatRuntimeModel";
import { useSessionViewSnapshotActions } from "../src/components/chat/useSessionViewSnapshotActions";
import { useSessionRequestOwnership } from "../src/components/chat/useSessionRequestOwnership";
import type { SessionViewHydrationControllerOptions } from "../src/components/chat/sessionViewHydrationPorts";
import type { ChatMessage, SessionHydrationSnapshot } from "../src/components/chat/types";

vi.mock("../src/lib/chatBridge", async original => ({ ...await original<object>(), getAgentRuntimeConfig: vi.fn(), getAgentContextUsage: vi.fn(), listAgentRuns: vi.fn(), getAgentState: vi.fn() }));
vi.mock("../src/components/chat/sessionTranscriptSync", () => ({ synchronizeSessionTranscript: vi.fn() }));
const config: AgentRuntimeConfig = { executionHost: "localUser", modelProviders: [], selectableModels: [], autoContinueAfterResumeWait: true, updatedAt: 3 };
const usage = (sessionId: string, updatedAt = 3): AgentContextUsageSummary => ({ sessionId, updatedAt, usedTokens: 25, maxContextTokens: 100, usedPercentage: 25, isCompacting: false });
const message = (id: string): ChatMessage => ({ id: "message-" + id, role: "user", text: "Body for " + id });
const snapshot = (id: string): SessionHydrationSnapshot => ({ messages: [message(id)], replayCursorsByAgentRunId: {}, pendingQuestionRequest: null, restoreMessageId: null, activeReplay: null });
function deferred<T>() { let resolve!: (value: T) => void; let reject!: (error: unknown) => void; const promise = new Promise<T>((ok, fail) => { resolve = ok; reject = fail; }); return { promise, resolve, reject }; }
const metadata = { config: vi.fn(), usage: vi.fn(), error: vi.fn(), preference: vi.fn() };
let actions: ReturnType<typeof useSessionViewSnapshotActions>;
function Harness({ id }: { id: string }) {
  const ownership = useSessionRequestOwnership(id);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const messagesRef = useRef(messages); messagesRef.current = messages;
  const visibleSessionIdRef = useRef(id); visibleSessionIdRef.current = id;
  const ports: SessionViewHydrationControllerOptions = {
    view: { currentSessionId: id, ...ownership, messagesRef, visibleSessionIdRef, transcriptViewRef: useRef(null), transcriptHistoryMessageCountRef: useRef(0), setMessages, setTranscriptHasOlder: vi.fn(), setSessionLoadError: vi.fn(), setEditingUserMessageId: vi.fn(), setEditingPrompt: vi.fn() },
    replay: { replayCursorsByAgentRunIdRef: useRef({}), verifiedReplayAgentRunIdsRef: useRef(new Set<string>()), visibleActiveReplayRef: useRef(null), persistVisibleSessionViewCache: vi.fn() },
    runtime: { setAutoContinueAfterResumeWait: metadata.preference, applyGlobalRuntimeConfig: metadata.config, applyContextUsage: metadata.usage, resetContextUsage: vi.fn(), reportRuntimeConfigError: metadata.error },
    question: { setPendingQuestion: vi.fn(), setPendingQuestionError: vi.fn() },
    stream: { getActiveStream: () => null, closeActiveStream: vi.fn(), setIsStreaming: vi.fn(), processStreamPayload: vi.fn(), startStreamForAssistant: vi.fn(), clearStreamEventHistory: vi.fn(), updateAssistantTurn: vi.fn(), flushAssistantTurnUpdates: vi.fn() },
    sessionOutcome: { pendingResolvedSessionRef: useRef(null), preserveResolvedSessionIdRef: useRef(null) },
  };
  actions = useSessionViewSnapshotActions(ports);
  return <div>{messages.map(item => <p key={item.id}>{item.role === "user" ? item.text : "Assistant"}</p>)}</div>;
}
beforeEach(() => {
  vi.clearAllMocks(); sessionViewCacheStore.clear();
  vi.mocked(listAgentRuns).mockResolvedValue({ agentRuns: [] });
  vi.mocked(getAgentState).mockResolvedValue({ session_id: "a", pending_question_count: 0, pending_questions: [], checkpoint: null });
  vi.mocked(synchronizeSessionTranscript).mockResolvedValue({ materializeMessages: () => [message("a")] } as unknown as Awaited<ReturnType<typeof synchronizeSessionTranscript>>);
});
afterEach(() => { cleanup(); sessionViewCacheStore.clear(); });

test("durable body becomes available while model configuration and context usage are still pending", async () => {
  const pendingConfig = deferred<AgentRuntimeConfig>(), pendingUsage = deferred<AgentContextUsageSummary>();
  vi.mocked(getAgentRuntimeConfig).mockReturnValue(pendingConfig.promise); vi.mocked(getAgentContextUsage).mockReturnValue(pendingUsage.promise);
  let body: SessionHydrationSnapshot | undefined;
  const request = buildSessionHydrationSnapshot("a", { yieldToUi: async () => {} }).then(value => { body = value; });
  try { await request; expect(body?.messages).toEqual([message("a")]); }
  finally { pendingConfig.resolve(config); pendingUsage.resolve(usage("a")); await request; }
});

test("configuration and context usage fill in independently after the body without replacing it", async () => {
  const pendingConfig = deferred<AgentRuntimeConfig>(), pendingUsage = deferred<AgentContextUsageSummary>();
  vi.mocked(getAgentRuntimeConfig).mockReturnValue(pendingConfig.promise); vi.mocked(getAgentContextUsage).mockReturnValue(pendingUsage.promise);
  render(<Harness id="a" />);
  act(() => actions.applyHydrationSnapshot(snapshot("a"), "a"));
  expect(screen.getByText("Body for a")).toBeTruthy();
  await act(async () => pendingUsage.resolve(usage("a")));
  expect(metadata.usage).toHaveBeenCalledWith("a", usage("a"));
  expect(metadata.config).not.toHaveBeenCalled();
  await act(async () => pendingConfig.resolve(config));
  expect(metadata.config).toHaveBeenCalledWith(config);
  expect(screen.getByText("Body for a")).toBeTruthy();
});

test("metadata from a previous visit cannot overwrite a newer visit to the same chat", async () => {
  const oldConfig = deferred<AgentRuntimeConfig>(), oldUsage = deferred<AgentContextUsageSummary>();
  vi.mocked(getAgentRuntimeConfig).mockReturnValueOnce(oldConfig.promise).mockResolvedValue(config);
  vi.mocked(getAgentContextUsage).mockReturnValueOnce(oldUsage.promise).mockImplementation(async id => usage(id));
  const view = render(<Harness id="a" />);
  act(() => actions.applyHydrationSnapshot(snapshot("a"), "a"));
  view.rerender(<Harness id="b" />);
  await act(async () => actions.applyHydrationSnapshot(snapshot("b"), "b"));
  view.rerender(<Harness id="a" />);
  await act(async () => actions.applyHydrationSnapshot(snapshot("a"), "a"));
  expect(metadata.config).toHaveBeenCalledWith(config);
  expect(metadata.usage).toHaveBeenLastCalledWith("a", usage("a"));
  metadata.config.mockClear(); metadata.usage.mockClear();
  await act(async () => { oldConfig.resolve({ ...config, updatedAt: 1 }); oldUsage.resolve(usage("a", 1)); });
  expect(metadata.config).not.toHaveBeenCalled(); expect(metadata.usage).not.toHaveBeenCalled();
  expect(screen.getByText("Body for a")).toBeTruthy();
});

test("an optional metadata failure leaves the readable body and its cache intact", async () => {
  vi.mocked(getAgentRuntimeConfig).mockRejectedValue(new Error("Model configuration unavailable"));
  vi.mocked(getAgentContextUsage).mockRejectedValue(new Error("Context information unavailable"));
  render(<Harness id="a" />);
  await act(async () => actions.applyHydrationSnapshot(snapshot("a"), "a"));
  expect(screen.getByText("Body for a")).toBeTruthy();
  expect(sessionViewCacheStore.get("a")?.snapshot.messages).toEqual([message("a")]);
  expect(metadata.error).toHaveBeenCalledWith(expect.stringContaining("Model configuration unavailable"));
});

test("runtime configuration can fill in before context usage without waiting for it", async () => {
  const pendingUsage = deferred<AgentContextUsageSummary>();
  vi.mocked(getAgentRuntimeConfig).mockResolvedValue(config);
  vi.mocked(getAgentContextUsage).mockReturnValue(pendingUsage.promise);
  render(<Harness id="a" />);
  await act(async () => actions.applyHydrationSnapshot(snapshot("a"), "a"));
  expect(metadata.config).toHaveBeenCalledWith(config);
  expect(metadata.usage).not.toHaveBeenCalled();
  expect(screen.getByText("Body for a")).toBeTruthy();
  await act(async () => pendingUsage.resolve(usage("a")));
  expect(metadata.usage).toHaveBeenCalledWith("a", usage("a"));
});

test("unmounting the view discards both pending metadata responses", async () => {
  const pendingConfig = deferred<AgentRuntimeConfig>(), pendingUsage = deferred<AgentContextUsageSummary>();
  vi.mocked(getAgentRuntimeConfig).mockReturnValue(pendingConfig.promise); vi.mocked(getAgentContextUsage).mockReturnValue(pendingUsage.promise);
  const view = render(<Harness id="a" />);
  act(() => actions.applyHydrationSnapshot(snapshot("a"), "a"));
  view.unmount();
  await act(async () => { pendingConfig.resolve(config); pendingUsage.resolve(usage("a")); });
  expect(metadata.config).not.toHaveBeenCalled(); expect(metadata.usage).not.toHaveBeenCalled();
});


test("the body still waits for authoritative pending-question state", async () => {
  const pendingState = deferred<Awaited<ReturnType<typeof getAgentState>>>();
  vi.mocked(getAgentState).mockReturnValueOnce(pendingState.promise);
  let available = false;
  const request = buildSessionHydrationSnapshot("a", { yieldToUi: async () => {} }).then(value => { available = true; return value; });
  await Promise.resolve();
  expect(available).toBe(false);
  pendingState.resolve({ session_id: "a", pending_question_count: 0, pending_questions: [], checkpoint: null });
  const body = await request;
  expect(body.messages).toEqual([message("a")]);
  expect(body.pendingQuestionRequest).toBeNull();
});
