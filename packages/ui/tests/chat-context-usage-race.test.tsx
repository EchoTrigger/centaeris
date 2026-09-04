import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type {
  AgentContextCompactResponse,
  AgentContextUsageSummary,
  AgentRuntimeConfig,
} from "../src/lib/chatBridge";
import type { SessionHydrationSnapshot } from "../src/components/chat/types";
import type { UiSession } from "../src/types/ui";

type DeferredRequest<T> = {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (reason?: unknown) => void;
};

type UsageRequest = DeferredRequest<AgentContextUsageSummary> & {
  sessionId: string;
};

type CompactRequest = DeferredRequest<AgentContextCompactResponse> & {
  sessionId: string;
};

type HydrationRequest = DeferredRequest<SessionHydrationSnapshot>;

type ComposerHarnessProps = {
  contextUsage: AgentContextUsageSummary | null;
  isCompacting: boolean;
  isStreaming: boolean;
  runtimeConfigError: string;
  onCompact: () => void;
};

const runtimeConfig: AgentRuntimeConfig = {
  executionHost: "localUser",
  autoContinueAfterResumeWait: false,
  modelProviderId: "provider",
  model: "model",
  modelThinkingMode: "low",
  modelProviders: [],
  selectableModels: [],
  updatedAt: 1,
};

const harness = vi.hoisted(() => ({
  composerProps: null as ComposerHarnessProps | null,
  usageRequests: [] as UsageRequest[],
  compactRequests: [] as CompactRequest[],
  hydrationRequests: new Map<string, HydrationRequest>(),
}));

vi.mock("../src/lib/chatBridge", () => ({
  answerAgentQuestion: vi.fn(),
  cancelAgentRun: vi.fn(),
  compactAgentContext: vi.fn((sessionId: string) => {
    let resolveRequest: (value: AgentContextCompactResponse) => void = () => {};
    let rejectRequest: (reason?: unknown) => void = () => {};
    const promise = new Promise<AgentContextCompactResponse>((resolve, reject) => {
      resolveRequest = resolve;
      rejectRequest = reject;
    });
    harness.compactRequests.push({
      sessionId,
      promise,
      resolve: resolveRequest,
      reject: rejectRequest,
    });
    return promise;
  }),
  createSession: vi.fn(),
  getAgentContextUsage: vi.fn((sessionId: string) => {
    let resolveRequest: (value: AgentContextUsageSummary) => void = () => {};
    let rejectRequest: (reason?: unknown) => void = () => {};
    const promise = new Promise<AgentContextUsageSummary>((resolve, reject) => {
      resolveRequest = resolve;
      rejectRequest = reject;
    });
    harness.usageRequests.push({
      sessionId,
      promise,
      resolve: resolveRequest,
      reject: rejectRequest,
    });
    return promise;
  }),
  getAgentRuntimeConfig: vi.fn(async () => runtimeConfig),
  getAgentState: vi.fn(),
  getSession: vi.fn(),
  getSessionProjection: vi.fn(),
  listAgentRuns: vi.fn(),
  openAgentStream: vi.fn(() => ({ close: vi.fn() })),
  replayAgentRunStream: vi.fn(),
  sendAgentInput: vi.fn(),
  sendAgentSupplement: vi.fn(),
  setAgentRuntimeConfig: vi.fn(async () => runtimeConfig),
}));

vi.mock("../src/components/chat/ChatComposer", () => ({
  ChatComposer: (props: ComposerHarnessProps) => {
    harness.composerProps = props;
    return <div data-testid="chat-composer" />;
  },
}));

vi.mock("../src/components/chat/chatRuntimeModel", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("../src/components/chat/chatRuntimeModel")>();
  return {
    ...actual,
    buildSessionHydrationSnapshot: vi.fn((sessionId: string) => {
      let resolveRequest: (snapshot: SessionHydrationSnapshot) => void = () => {};
      let rejectRequest: (reason?: unknown) => void = () => {};
      const promise = new Promise<SessionHydrationSnapshot>((resolve, reject) => {
        resolveRequest = resolve;
        rejectRequest = reject;
      });
      harness.hydrationRequests.set(sessionId, {
        promise,
        resolve: resolveRequest,
        reject: rejectRequest,
      });
      return promise;
    }),
  };
});

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
  sessionKind: "main",
});

const makeUsage = (
  sessionId: string,
  updatedAt: number,
  usedTokens: number,
): AgentContextUsageSummary => ({
  sessionId,
  usedTokens,
  maxContextTokens: 100,
  usedPercentage: usedTokens,
  updatedAt,
  isCompacting: false,
});

const makeHydrationSnapshot = (
  sessionId: string,
  contextUsage: AgentContextUsageSummary | null,
): SessionHydrationSnapshot => ({
  messages: [
    {
      id: `message-${sessionId}`,
      role: "user",
      text: `from ${sessionId}`,
    },
  ],
  runtimeConfig,
  contextUsage,
  resolvedAutoContinueAfterResumeWait: false,
  replayCursorsByAgentRunId: {},
  pendingQuestionRequest: null,
  restoreMessageId: null,
  activeReplay: null,
});

const getComposerProps = (): ComposerHarnessProps => {
  if (!harness.composerProps) {
    throw new Error("ChatComposer has not rendered");
  }
  return harness.composerProps;
};

const getHydrationRequest = (sessionId: string): HydrationRequest => {
  const request = harness.hydrationRequests.get(sessionId);
  if (!request) {
    throw new Error(`missing hydration request for ${sessionId}`);
  }
  return request;
};

const renderSession = async (sessionId: string): Promise<ReactTestRenderer> => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(
      <ChatArea
        currentSession={makeSession(sessionId)}
        currentSessionId={sessionId}
        workspaceName="Workspace"
      />,
    );
  });
  if (!renderer) {
    throw new Error("ChatArea did not render");
  }
  return renderer;
};

const resolveHydration = async (
  sessionId: string,
  contextUsage: AgentContextUsageSummary | null,
): Promise<void> => {
  const request = getHydrationRequest(sessionId);
  await act(async () => {
    request.resolve(makeHydrationSnapshot(sessionId, contextUsage));
    await request.promise;
  });
};

const triggerCompact = async (): Promise<void> => {
  await act(async () => {
    getComposerProps().onCompact();
    await Promise.resolve();
  });
};

const resolveCompact = async (
  request: CompactRequest,
  compacted: boolean,
): Promise<void> => {
  await act(async () => {
    request.resolve({ sessionId: request.sessionId, compacted });
    await request.promise;
    await Promise.resolve();
  });
};

const resolveUsage = async (
  request: UsageRequest,
  usage: AgentContextUsageSummary,
): Promise<void> => {
  await act(async () => {
    request.resolve(usage);
    await request.promise;
    await Promise.resolve();
  });
};

beforeEach(() => {
  harness.composerProps = null;
  harness.usageRequests.length = 0;
  harness.compactRequests.length = 0;
  harness.hydrationRequests.clear();
  sessionViewCacheStore.clear();
  useChatViewStore.getState().clear();
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: {
      localStorage: {
        getItem: vi.fn(() => null),
        setItem: vi.fn(),
      },
      requestAnimationFrame: vi.fn(() => 1),
      cancelAnimationFrame: vi.fn(),
      setTimeout: vi.fn(() => 1),
      clearTimeout: vi.fn(),
    },
  });
});

afterEach(() => {
  sessionViewCacheStore.clear();
  useChatViewStore.getState().clear();
  if (originalWindowDescriptor) {
    Object.defineProperty(globalThis, "window", originalWindowDescriptor);
  } else {
    Reflect.deleteProperty(globalThis, "window");
  }
});

test("successful manual compaction refreshes usage and clears the pending state", async () => {
  const initialUsage = makeUsage("session", 1, 20);
  const refreshedUsage = makeUsage("session", 2, 10);
  const renderer = await renderSession("session");
  await resolveHydration("session", initialUsage);

  await triggerCompact();
  expect(harness.compactRequests).toHaveLength(1);
  expect(getComposerProps().isCompacting).toBe(true);

  await resolveCompact(harness.compactRequests[0], true);
  expect(harness.usageRequests).toHaveLength(1);
  expect(harness.usageRequests[0].sessionId).toBe("session");
  await resolveUsage(harness.usageRequests[0], refreshedUsage);

  expect(getComposerProps().contextUsage).toEqual(refreshedUsage);
  expect(getComposerProps().runtimeConfigError).toBe("");
  expect(getComposerProps().isCompacting).toBe(false);
  await act(async () => renderer.unmount());
});

test("a no-op compaction reports the existing message and still refreshes usage", async () => {
  const usage = makeUsage("session", 1, 20);
  const renderer = await renderSession("session");
  await resolveHydration("session", usage);

  await triggerCompact();
  await resolveCompact(harness.compactRequests[0], false);
  expect(harness.usageRequests).toHaveLength(1);
  await resolveUsage(harness.usageRequests[0], usage);

  expect(getComposerProps().runtimeConfigError).toBe(
    "Not enough conversation history to compact.",
  );
  expect(getComposerProps().isCompacting).toBe(false);
  await act(async () => renderer.unmount());
});

test("a compaction failure reports the error and does not request usage", async () => {
  const renderer = await renderSession("session");
  await resolveHydration("session", makeUsage("session", 1, 20));

  await triggerCompact();
  const request = harness.compactRequests[0];
  await act(async () => {
    request.reject(new Error("compact unavailable"));
    await expect(request.promise).rejects.toThrow("compact unavailable");
    await Promise.resolve();
  });

  expect(harness.usageRequests).toHaveLength(0);
  expect(getComposerProps().runtimeConfigError).toContain("compact unavailable");
  expect(getComposerProps().isCompacting).toBe(false);
  await act(async () => renderer.unmount());
});

test("a failed usage refresh preserves the last canonical snapshot", async () => {
  const usage = makeUsage("session", 1, 20);
  const renderer = await renderSession("session");
  await resolveHydration("session", usage);

  await triggerCompact();
  await resolveCompact(harness.compactRequests[0], true);
  const request = harness.usageRequests[0];
  await act(async () => {
    request.reject(new Error("usage unavailable"));
    await expect(request.promise).rejects.toThrow("usage unavailable");
    await Promise.resolve();
  });

  expect(getComposerProps().contextUsage).toEqual(usage);
  expect(getComposerProps().runtimeConfigError).toBe("");
  expect(getComposerProps().isCompacting).toBe(false);
  await act(async () => renderer.unmount());
});

test("streaming sessions do not start manual compaction", async () => {
  const sessionId = "streaming";
  sessionViewCacheStore.write({
    sessionId,
    snapshot: {
      messages: makeHydrationSnapshot(sessionId, null).messages,
      contextUsage: null,
      autoContinueAfterResumeWait: false,
      pendingQuestion: null,
      pendingQuestionError: "",
      activeReplay: {
        messageId: "assistant-streaming",
        agentRunId: "run-streaming",
      },
    },
  });
  const renderer = await renderSession(sessionId);
  expect(getComposerProps().isStreaming).toBe(true);

  await triggerCompact();

  expect(harness.compactRequests).toHaveLength(0);
  await act(async () => renderer.unmount());
});

test("two compact actions in the same turn start only one request", async () => {
  const renderer = await renderSession("session");
  await resolveHydration("session", makeUsage("session", 1, 20));
  const onCompact = getComposerProps().onCompact;

  await act(async () => {
    onCompact();
    onCompact();
    await Promise.resolve();
  });

  expect(harness.compactRequests).toHaveLength(1);
  await act(async () => renderer.unmount());
});

test("a usage response from the previous session cannot overwrite the visible session", async () => {
  const renderer = await renderSession("a");
  await resolveHydration("a", makeUsage("a", 1, 10));
  await triggerCompact();
  await resolveCompact(harness.compactRequests[0], true);
  const staleUsageRequest = harness.usageRequests[0];

  await act(async () => {
    renderer.update(
      <ChatArea
        currentSession={makeSession("b")}
        currentSessionId="b"
        workspaceName="Workspace"
      />,
    );
  });
  const visibleUsage = makeUsage("b", 2, 20);
  await resolveHydration("b", visibleUsage);
  await resolveUsage(staleUsageRequest, makeUsage("a", 3, 30));

  expect(getComposerProps().contextUsage).toEqual(visibleUsage);
  await act(async () => renderer.unmount());
});

test("a compaction failure from the previous session is not shown in the visible session", async () => {
  const renderer = await renderSession("a");
  await resolveHydration("a", makeUsage("a", 1, 10));
  await triggerCompact();
  const staleCompactRequest = harness.compactRequests[0];

  await act(async () => {
    renderer.update(
      <ChatArea
        currentSession={makeSession("b")}
        currentSessionId="b"
        workspaceName="Workspace"
      />,
    );
  });
  await resolveHydration("b", makeUsage("b", 2, 20));
  await act(async () => {
    staleCompactRequest.reject(new Error("old session failed"));
    await expect(staleCompactRequest.promise).rejects.toThrow(
      "old session failed",
    );
    await Promise.resolve();
  });

  expect(getComposerProps().runtimeConfigError).toBe("");
  expect(getComposerProps().isCompacting).toBe(false);
  await act(async () => renderer.unmount());
});

test("an older hydration snapshot cannot overwrite refreshed usage", async () => {
  const renderer = await renderSession("session");
  const hydration = getHydrationRequest("session");
  await triggerCompact();
  await resolveCompact(harness.compactRequests[0], true);
  const refreshedUsage = makeUsage("session", 2, 10);
  await resolveUsage(harness.usageRequests[0], refreshedUsage);

  await act(async () => {
    hydration.resolve(
      makeHydrationSnapshot("session", makeUsage("session", 1, 20)),
    );
    await hydration.promise;
  });

  expect(getComposerProps().contextUsage).toEqual(refreshedUsage);
  await act(async () => renderer.unmount());
});
