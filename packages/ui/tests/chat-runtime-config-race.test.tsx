import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type {
  AgentRuntimeConfig,
  ModelThinkingMode,
  SelectableModel,
} from "../src/lib/chatBridge";
import type { SessionHydrationSnapshot } from "../src/components/chat/types";
import type { UiSession } from "../src/types/ui";

type ConfigRequest = {
  promise: Promise<AgentRuntimeConfig>;
  resolve: (config: AgentRuntimeConfig) => void;
};

type ConfigUpdateRequest = ConfigRequest & {
  input: Record<string, unknown>;
  reject: (reason?: unknown) => void;
};

type HydrationRequest = {
  promise: Promise<SessionHydrationSnapshot>;
  resolve: (snapshot: SessionHydrationSnapshot) => void;
};

type ComposerHarnessProps = {
  modelRuntimeSummary: string;
  reasoningEffort: ModelThinkingMode | null;
  runtimeConfigError: string;
  onModelSelect: (configured: SelectableModel) => void;
  onReasoningEffortSelect: (effort: ModelThinkingMode) => void;
};

const harness = vi.hoisted(() => ({
  composerProps: null as ComposerHarnessProps | null,
  configRequests: [] as ConfigRequest[],
  configUpdates: [] as ConfigUpdateRequest[],
  hydrationRequests: new Map<string, HydrationRequest>(),
}));

vi.mock("../src/lib/chatBridge", () => ({
  answerAgentQuestion: vi.fn(),
  cancelAgentRun: vi.fn(),
  compactAgentContext: vi.fn(),
  createSession: vi.fn(),
  getAgentContextUsage: vi.fn(),
  getAgentRuntimeConfig: vi.fn(() => {
    let resolveConfig: (config: AgentRuntimeConfig) => void = () => {};
    const promise = new Promise<AgentRuntimeConfig>((resolve) => {
      resolveConfig = resolve;
    });
    harness.configRequests.push({ promise, resolve: resolveConfig });
    return promise;
  }),
  getAgentState: vi.fn(),
  getSession: vi.fn(),
  getSessionProjection: vi.fn(),
  listAgentRuns: vi.fn(),
  openAgentStream: vi.fn(() => ({ close: vi.fn() })),
  replayAgentRunStream: vi.fn(),
  sendAgentInput: vi.fn(),
  sendAgentSupplement: vi.fn(),
  setAgentRuntimeConfig: vi.fn((input: Record<string, unknown>) => {
    let resolveConfig: (config: AgentRuntimeConfig) => void = () => {};
    let rejectConfig: (reason?: unknown) => void = () => {};
    const promise = new Promise<AgentRuntimeConfig>((resolve, reject) => {
      resolveConfig = resolve;
      rejectConfig = reject;
    });
    harness.configUpdates.push({
      input,
      promise,
      resolve: resolveConfig,
      reject: rejectConfig,
    });
    return promise;
  }),
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
      let resolveSnapshot: (snapshot: SessionHydrationSnapshot) => void = () => {};
      const promise = new Promise<SessionHydrationSnapshot>((resolve) => {
        resolveSnapshot = resolve;
      });
      harness.hydrationRequests.set(sessionId, {
        promise,
        resolve: resolveSnapshot,
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

const makeConfig = (
  modelProviderId: string,
  model: string,
  updatedAt: number,
  modelThinkingMode: ModelThinkingMode = "low",
  selectableModels: SelectableModel[] = [],
): AgentRuntimeConfig => ({
  executionHost: "localUser",
  autoContinueAfterResumeWait: false,
  modelProviderId,
  model,
  modelThinkingMode,
  modelProviders: [],
  selectableModels,
  updatedAt,
});

const makeSelectableModel = (
  providerId: string,
  model: string,
): SelectableModel => ({
  providerId,
  providerName: providerId,
  model,
  modelThinkingModes: ["low", "medium", "high"],
});

const makeSession = (id: string): UiSession => ({
  id,
  title: `Session ${id}`,
  messageCount: 1,
  sessionKind: "main",
});

const makeHydrationSnapshot = (
  sessionId: string,
  config: AgentRuntimeConfig,
): SessionHydrationSnapshot => ({
  messages: [
    {
      id: `message-${sessionId}`,
      role: "user",
      text: `from ${sessionId}`,
    },
  ],
  runtimeConfig: config,
  contextUsage: null,
  resolvedAutoContinueAfterResumeWait: false,
  replayCursorsByAgentRunId: {},
  pendingQuestionRequest: null,
  restoreMessageId: null,
  activeReplay: null,
});

const getHydrationRequest = (sessionId: string): HydrationRequest => {
  const request = harness.hydrationRequests.get(sessionId);
  if (!request) {
    throw new Error(`missing hydration request for ${sessionId}`);
  }
  return request;
};

const getComposerProps = (): ComposerHarnessProps => {
  if (!harness.composerProps) {
    throw new Error("ChatComposer has not rendered");
  }
  return harness.composerProps;
};

beforeEach(() => {
  harness.composerProps = null;
  harness.configRequests.length = 0;
  harness.configUpdates.length = 0;
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

test("an older runtime config read cannot overwrite a newer revision", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(
      <ChatArea
        currentSession={null}
        currentSessionId={null}
        workspaceName="Workspace"
        runtimeConfigRevision={0}
      />,
    );
  });
  expect(harness.configRequests).toHaveLength(1);

  await act(async () => {
    renderer!.update(
      <ChatArea
        currentSession={null}
        currentSessionId={null}
        workspaceName="Workspace"
        runtimeConfigRevision={1}
      />,
    );
  });
  expect(harness.configRequests).toHaveLength(2);

  const [olderRequest, newerRequest] = harness.configRequests;
  await act(async () => {
    newerRequest.resolve(makeConfig("new-provider", "new-model", 2));
    await newerRequest.promise;
  });
  expect(getComposerProps().modelRuntimeSummary).toBe(
    "new-model · new-provider",
  );

  await act(async () => {
    olderRequest.resolve(makeConfig("old-provider", "old-model", 1));
    await olderRequest.promise;
  });
  expect(getComposerProps().modelRuntimeSummary).toBe(
    "new-model · new-provider",
  );

  await act(async () => renderer!.unmount());
});

test("a model selection wins over an older refresh already in flight", async () => {
  let renderer: ReactTestRenderer | null = null;
  const firstModel = makeSelectableModel("provider-a", "model-a");
  const selectedModel = makeSelectableModel("provider-b", "model-b");
  await act(async () => {
    renderer = create(
      <ChatArea
        currentSession={null}
        currentSessionId={null}
        workspaceName="Workspace"
        runtimeConfigRevision={0}
      />,
    );
  });
  const initialRequest = harness.configRequests[0];
  await act(async () => {
    initialRequest.resolve(
      makeConfig("provider-a", "model-a", 1, "low", [
        firstModel,
        selectedModel,
      ]),
    );
    await initialRequest.promise;
  });

  await act(async () => {
    renderer!.update(
      <ChatArea
        currentSession={null}
        currentSessionId={null}
        workspaceName="Workspace"
        runtimeConfigRevision={1}
      />,
    );
  });
  const staleRefresh = harness.configRequests[1];
  await act(async () => getComposerProps().onModelSelect(selectedModel));
  expect(harness.configUpdates).toHaveLength(1);
  const selection = harness.configUpdates[0];
  expect(selection.input).toEqual({
    modelProviderId: "provider-b",
    model: "model-b",
  });

  await act(async () => {
    selection.resolve(
      makeConfig("provider-b", "model-b", 3, "low", [
        firstModel,
        selectedModel,
      ]),
    );
    await selection.promise;
  });
  await act(async () => {
    staleRefresh.resolve(
      makeConfig("provider-a", "model-a", 2, "low", [
        firstModel,
        selectedModel,
      ]),
    );
    await staleRefresh.promise;
  });
  expect(getComposerProps().modelRuntimeSummary).toBe(
    "model-b · provider-b",
  );

  await act(async () => renderer!.unmount());
});

test("reasoning updates report the latest failure and clear it on success", async () => {
  let renderer: ReactTestRenderer | null = null;
  const selectedModel = makeSelectableModel("provider", "model");
  await act(async () => {
    renderer = create(
      <ChatArea
        currentSession={null}
        currentSessionId={null}
        workspaceName="Workspace"
      />,
    );
  });
  const initialRequest = harness.configRequests[0];
  await act(async () => {
    initialRequest.resolve(
      makeConfig("provider", "model", 1, "low", [selectedModel]),
    );
    await initialRequest.promise;
  });

  await act(async () => getComposerProps().onReasoningEffortSelect("high"));
  const failedUpdate = harness.configUpdates[0];
  expect(failedUpdate.input).toEqual({ modelThinkingMode: "high" });
  await act(async () => {
    failedUpdate.reject(new Error("config write failed"));
    await expect(failedUpdate.promise).rejects.toThrow("config write failed");
    await Promise.resolve();
  });
  expect(getComposerProps().runtimeConfigError).toContain(
    "config write failed",
  );

  await act(async () => getComposerProps().onReasoningEffortSelect("medium"));
  const successfulUpdate = harness.configUpdates[1];
  expect(successfulUpdate.input).toEqual({ modelThinkingMode: "medium" });
  await act(async () => {
    successfulUpdate.resolve(
      makeConfig("provider", "model", 2, "medium", [selectedModel]),
    );
    await successfulUpdate.promise;
  });
  expect(getComposerProps().reasoningEffort).toBe("medium");
  expect(getComposerProps().runtimeConfigError).toBe("");

  await act(async () => renderer!.unmount());
});

test("an older hydration snapshot cannot cancel a model selection in flight", async () => {
  let renderer: ReactTestRenderer | null = null;
  const firstModel = makeSelectableModel("provider-a", "model-a");
  const selectedModel = makeSelectableModel("provider-b", "model-b");
  const initialConfig = makeConfig("provider-a", "model-a", 1, "low", [
    firstModel,
    selectedModel,
  ]);
  await act(async () => {
    renderer = create(
      <ChatArea
        currentSession={makeSession("session")}
        currentSessionId="session"
        workspaceName="Workspace"
      />,
    );
  });
  const initialRequest = harness.configRequests[0];
  await act(async () => {
    initialRequest.resolve(initialConfig);
    await initialRequest.promise;
  });

  await act(async () => getComposerProps().onModelSelect(selectedModel));
  const selection = harness.configUpdates[0];
  const hydration = getHydrationRequest("session");
  await act(async () => {
    hydration.resolve(makeHydrationSnapshot("session", initialConfig));
    await hydration.promise;
  });
  await act(async () => {
    selection.resolve(
      makeConfig("provider-b", "model-b", 2, "low", [
        firstModel,
        selectedModel,
      ]),
    );
    await selection.promise;
  });

  expect(getComposerProps().modelRuntimeSummary).toBe(
    "model-b · provider-b",
  );

  await act(async () => renderer!.unmount());
});
