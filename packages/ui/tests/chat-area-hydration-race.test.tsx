import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type {
  AgentRunListResponse,
  AgentRunLiveSnapshotResponse,
  AgentRunSummary,
  AgentRuntimeConfig,
  AgentStreamPayload,
  SessionData,
} from "../src/lib/chatBridge";
import type { SessionHydrationSnapshot } from "../src/components/chat/types";
import type { UiSession } from "../src/types/ui";

type HydrationRequest = {
  promise: Promise<SessionHydrationSnapshot>;
  resolve: (snapshot: SessionHydrationSnapshot) => void;
  reject: (reason?: unknown) => void;
};

type CapturedStream = {
  agentRunId: string;
  onMessage: (payload: AgentStreamPayload) => void;
  close: ReturnType<typeof vi.fn>;
};

const harness = vi.hoisted(() => ({
  hydrationRequests: new Map<string, HydrationRequest>(),
  streams: [] as CapturedStream[],
  getSession: vi.fn<(sessionId: string) => Promise<SessionData>>(),
  listAgentRuns: vi.fn<() => Promise<AgentRunListResponse>>(),
  getAgentRunLiveSnapshot:
    vi.fn<() => Promise<AgentRunLiveSnapshotResponse>>(),
}));

const runtimeConfig: AgentRuntimeConfig = {
  executionHost: "localUser",
  autoContinueAfterResumeWait: false,
  modelProviders: [],
  selectableModels: [],
  updatedAt: 1,
};

vi.mock("../src/lib/chatBridge", () => ({
  answerAgentQuestion: vi.fn(),
  cancelAgentRun: vi.fn(),
  compactAgentContext: vi.fn(),
  createSession: vi.fn(),
  getAgentContextUsage: vi.fn(),
  getAgentRuntimeConfig: vi.fn(async () => runtimeConfig),
  getAgentState: vi.fn(),
  getSession: harness.getSession,
  listAgentRuns: harness.listAgentRuns,
  openAgentStream: vi.fn(
    (
      agentRunId: string,
      onMessage: (payload: AgentStreamPayload) => void,
    ) => {
      const close = vi.fn();
      harness.streams.push({ agentRunId, onMessage, close });
      return { close };
    },
  ),
  getAgentRunLiveSnapshot: harness.getAgentRunLiveSnapshot,
  sendAgentInput: vi.fn(),
  sendAgentSupplement: vi.fn(),
  setAgentRuntimeConfig: vi.fn(),
}));

vi.mock("../src/components/chat/chatRuntimeModel", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("../src/components/chat/chatRuntimeModel")>();
  return {
    ...actual,
    buildSessionHydrationSnapshot: vi.fn((sessionId: string) => {
      let resolveSnapshot: (snapshot: SessionHydrationSnapshot) => void = () => {};
      let rejectSnapshot: (reason?: unknown) => void = () => {};
      const promise = new Promise<SessionHydrationSnapshot>((resolve, reject) => {
        resolveSnapshot = resolve;
        rejectSnapshot = reject;
      });
      harness.hydrationRequests.set(sessionId, {
        promise,
        resolve: resolveSnapshot,
        reject: rejectSnapshot,
      });
      return promise;
    }),
  };
});

vi.mock("../src/components/chat/ChatComposer", () => ({
  ChatComposer: () => <div data-testid="chat-composer" />,
}));

vi.mock("../src/components/chat/VirtualMessageList", () => ({
  VirtualMessageList: () => <div data-testid="virtual-message-list" />,
}));

vi.mock("../src/components/chat/ChatPendingPanels", () => ({
  PendingQuestionPanel: () => <div data-testid="pending-question" />,
}));

vi.mock("../src/host/hostBridge", () => ({
  isNativeHostRuntime: () => false,
}));

import { ChatArea } from "../src/components/chat/ChatArea";
import { sessionViewCacheStore } from "../src/components/chat/chatRuntimeCore";
import { useChatViewStore } from "../src/components/chat/chatViewStore";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const originalWindowDescriptor = Object.getOwnPropertyDescriptor(
  globalThis,
  "window",
);

const makeSession = (id: string): UiSession => ({
  id,
  title: `Session ${id}`,
  messageCount: 1,
  cwd: `D:\\Workspace\\${id}`,
  sessionKind: "main",
});

const makeSnapshot = (sessionId: string): SessionHydrationSnapshot => ({
  messages: [
    {
      id: `message-${sessionId}`,
      role: "user",
      text: `from ${sessionId}`,
      timestamp: 1,
    },
  ],
  runtimeConfig,
  contextUsage: null,
  resolvedAutoContinueAfterResumeWait: false,
  replayCursorsByAgentRunId: {},
  pendingQuestionRequest: null,
  restoreMessageId: null,
  activeReplay: null,
});

const makeAgentRun = (
  sessionId: string,
  agentRunId: string,
  status: "running" | "completed" = "completed",
): AgentRunSummary => ({
  agentRunId,
  sessionId,
  turnId: `turn-${agentRunId}`,
  status,
  unread: false,
  startedAtMs: 1,
  updatedAtMs: 2,
});

const getHydrationRequest = (sessionId: string): HydrationRequest => {
  const request = harness.hydrationRequests.get(sessionId);
  if (!request) {
    throw new Error(`missing hydration request for ${sessionId}`);
  }
  return request;
};

beforeEach(() => {
  harness.hydrationRequests.clear();
  harness.streams.length = 0;
  harness.getSession.mockReset();
  harness.listAgentRuns.mockReset();
  harness.getAgentRunLiveSnapshot.mockReset();
  harness.listAgentRuns.mockResolvedValue({ agentRuns: [] });
  sessionViewCacheStore.clear();
  useChatViewStore.getState().clear();
  vi.useFakeTimers();
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: {
      localStorage: {
        getItem: vi.fn(() => null),
        setItem: vi.fn(),
      },
      requestAnimationFrame: vi.fn((callback: FrameRequestCallback) => {
        callback(0);
        return 1;
      }),
      cancelAnimationFrame: vi.fn(),
      setTimeout: globalThis.setTimeout.bind(globalThis),
      clearTimeout: globalThis.clearTimeout.bind(globalThis),
    },
  });
});

afterEach(() => {
  sessionViewCacheStore.clear();
  useChatViewStore.getState().clear();
  vi.useRealTimers();
  if (originalWindowDescriptor) {
    Object.defineProperty(globalThis, "window", originalWindowDescriptor);
  } else {
    Reflect.deleteProperty(globalThis, "window");
  }
});

test("a late hydration result cannot overwrite a newer selected session", async () => {
  let renderer: ReactTestRenderer | null = null;

  await act(async () => {
    renderer = create(
      <ChatArea
        currentSession={makeSession("a")}
        currentSessionId="a"
        workspaceName="Workspace"
        workspaceRoot="D:\\Workspace"
      />,
    );
  });
  const requestA = getHydrationRequest("a");

  await act(async () => {
    renderer!.update(
      <ChatArea
        currentSession={makeSession("b")}
        currentSessionId="b"
        workspaceName="Workspace"
        workspaceRoot="D:\\Workspace"
      />,
    );
  });
  const requestB = getHydrationRequest("b");

  await act(async () => {
    requestB.resolve(makeSnapshot("b"));
    await requestB.promise;
  });

  expect(useChatViewStore.getState().messageIds).toEqual(["message-b"]);
  expect(
    sessionViewCacheStore.get("b")?.snapshot.messages.map(({ id }) => id),
  ).toEqual(["message-b"]);

  await act(async () => {
    requestA.resolve(makeSnapshot("a"));
    await requestA.promise;
  });

  expect(useChatViewStore.getState().messageIds).toEqual(["message-b"]);
  expect(sessionViewCacheStore.get("a")).toBeNull();
  expect(
    sessionViewCacheStore.get("b")?.snapshot.messages.map(({ id }) => id),
  ).toEqual(["message-b"]);

  await act(async () => renderer!.unmount());
});

test("a hydration failure clears stale state and renders the session error", async () => {
  let renderer: ReactTestRenderer | null = null;

  await act(async () => {
    renderer = create(
      <ChatArea
        currentSession={makeSession("broken")}
        currentSessionId="broken"
        workspaceName="Workspace"
        workspaceRoot="D:\\Workspace"
      />,
    );
  });
  const request = getHydrationRequest("broken");
  sessionViewCacheStore.write({
    sessionId: "broken",
    snapshot: {
      messages: makeSnapshot("stale").messages,
      contextUsage: null,
      autoContinueAfterResumeWait: false,
      pendingQuestion: null,
      pendingQuestionError: "",
      activeReplay: null,
    },
  });
  expect(sessionViewCacheStore.get("broken")).not.toBeNull();

  await act(async () => {
    request.reject(new Error("projection unavailable"));
    await expect(request.promise).rejects.toThrow("projection unavailable");
    await Promise.resolve();
  });

  expect(sessionViewCacheStore.get("broken")).toBeNull();
  expect(useChatViewStore.getState().messageIds).toEqual([]);
  const alert = renderer!.root.findByProps({ role: "alert" });
  expect(alert.findByType("h2").children.join(" ")).toBe("Unable to load conversation");
  expect(alert.findByType("p").children.join(" ")).toContain(
    "projection unavailable",
  );
  expect(
    renderer!.root.findAllByProps({ "data-testid": "chat-composer" }),
  ).toHaveLength(0);

  await act(async () => renderer!.unmount());
});

test("a seed replay event is not applied again when the live stream repeats it", async () => {
  let renderer: ReactTestRenderer | null = null;
  const sessionId = "active";
  const agentRunId = "run-active";
  const assistantMessageId = "assistant-active";
  const seedPayload: AgentStreamPayload = {
    type: "session_event",
    agentRunId,
    event: {
      id: "seed-delta",
      type: "ModelTextDelta",
      at: 10,
      sessionId,
      turnId: "turn-active",
      taskId: agentRunId,
      parentTaskId: "turn-active",
      visibility: "user",
      payload: { delta: "seed" },
    },
  };

  await act(async () => {
    renderer = create(
      <ChatArea
        currentSession={makeSession(sessionId)}
        currentSessionId={sessionId}
        workspaceName="Workspace"
        workspaceRoot="D:\\Workspace"
      />,
    );
  });
  const request = getHydrationRequest(sessionId);
  const snapshot = makeSnapshot(sessionId);
  snapshot.messages.push({
    id: assistantMessageId,
    role: "assistant",
    turn: {
      id: "turn-active",
      agentRunId,
      chunks: [],
      finalAnswer: "seed",
      isStreaming: true,
      startedAtMs: 1,
    },
  });
  snapshot.replayCursorsByAgentRunId = { [agentRunId]: 1 };
  snapshot.activeReplay = {
    messageId: assistantMessageId,
    agentRunId,
    status: "running",
    seedPayloads: [seedPayload],
  };

  await act(async () => {
    request.resolve(snapshot);
    await request.promise;
  });
  const stream = harness.streams[0];
  if (!stream) {
    throw new Error("active hydration did not attach a stream");
  }

  await act(async () => {
    stream.onMessage(seedPayload);
    stream.onMessage({
      type: "session_event",
      agentRunId,
      event: {
        id: "terminal-active",
        type: "AgentRunCompleted",
        at: 20,
        sessionId,
        turnId: "turn-active",
        taskId: agentRunId,
        parentTaskId: "turn-active",
        visibility: "internal",
        payload: { doneReason: "finalized" },
      },
    });
    await Promise.resolve();
  });

  const assistant = useChatViewStore.getState().messageById[assistantMessageId];
  expect(assistant?.role).toBe("assistant");
  if (!assistant || assistant.role !== "assistant") {
    throw new Error("missing hydrated assistant message");
  }
  expect(assistant.turn.finalAnswer).toBe("seed");
  expect(stream.close).toHaveBeenCalledTimes(1);

  await act(async () => renderer!.unmount());
});

test("a cached page refresh resolving after a session switch cannot overwrite the visible view", async () => {
  let renderer: ReactTestRenderer | null = null;
  sessionViewCacheStore.write({
    sessionId: "cached",
    snapshot: {
      messages: makeSnapshot("cached").messages,
      contextUsage: null,
      autoContinueAfterResumeWait: false,
      pendingQuestion: null,
      pendingQuestionError: "",
      activeReplay: null,
    },
  });

  await act(async () => {
    renderer = create(
      <ChatArea
        currentSession={makeSession("cached")}
        currentSessionId="cached"
        workspaceName="Workspace"
        workspaceRoot="D:\\Workspace"
      />,
    );
  });
  expect(useChatViewStore.getState().messageIds).toEqual(["message-cached"]);
  const cachedRequest = getHydrationRequest("cached");
  expect(harness.getSession).not.toHaveBeenCalled();
  expect(harness.getAgentRunLiveSnapshot).not.toHaveBeenCalled();

  await act(async () => {
    renderer!.update(
      <ChatArea
        currentSession={makeSession("new")}
        currentSessionId="new"
        workspaceName="Workspace"
        workspaceRoot="D:\\Workspace"
      />,
    );
  });
  const newRequest = getHydrationRequest("new");
  await act(async () => {
    newRequest.resolve(makeSnapshot("new"));
    await newRequest.promise;
  });

  await act(async () => {
    cachedRequest.resolve(makeSnapshot("cached-refresh"));
    await cachedRequest.promise;
    await Promise.resolve();
  });

  expect(useChatViewStore.getState().messageIds).toEqual(["message-new"]);
  expect(sessionViewCacheStore.get("new")?.snapshot.messages).toEqual(
    makeSnapshot("new").messages,
  );

  await act(async () => renderer!.unmount());
});

test("a cached snapshot is refreshed through the bounded transcript page path", async () => {
  let renderer: ReactTestRenderer | null = null;
  const sessionId = "missing-tools";
  const agentRunId = "run-missing-tools";
  harness.listAgentRuns.mockResolvedValue({
    agentRuns: [makeAgentRun(sessionId, agentRunId)],
  });
  sessionViewCacheStore.write({
    sessionId,
    snapshot: {
      messages: makeSnapshot(sessionId).messages,
      contextUsage: null,
      autoContinueAfterResumeWait: false,
      pendingQuestion: null,
      pendingQuestionError: "",
      activeReplay: null,
    },
    replayCursorsByAgentRunId: { [agentRunId]: 0 },
    verifiedReplayAgentRunIds: [agentRunId],
  });

  await act(async () => {
    renderer = create(
      <ChatArea
        currentSession={makeSession(sessionId)}
        currentSessionId={sessionId}
        workspaceName="Workspace"
        workspaceRoot="D:\\Workspace"
      />,
    );
    await Promise.resolve();
    await Promise.resolve();
  });
  const fullReplayRequest = getHydrationRequest(sessionId);
  const fullSnapshot = makeSnapshot(sessionId);
  fullSnapshot.messages[0] = {
    id: "message-full-replay",
    role: "user",
    text: "restored from durable log",
    timestamp: 2,
  };

  await act(async () => {
    fullReplayRequest.resolve(fullSnapshot);
    await fullReplayRequest.promise;
    await Promise.resolve();
  });

  expect(harness.getAgentRunLiveSnapshot).not.toHaveBeenCalled();
  expect(harness.getSession).not.toHaveBeenCalled();
  expect(useChatViewStore.getState().messageIds).toEqual([
    "message-full-replay",
  ]);
  expect(
    sessionViewCacheStore.get(sessionId)?.snapshot.messages.map(({ id }) => id),
  ).toEqual(["message-full-replay"]);

  await act(async () => renderer?.unmount());
});
