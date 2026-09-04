import { Profiler, type ProfilerOnRenderCallback } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type {
  AgentContextUsageSummary,
  AgentRuntimeConfig,
  AgentStreamPayload,
  ModelThinkingMode,
  SelectableModel,
} from "../src/lib/chatBridge";
import type {
  AssistantExecutionTurn,
  ChatMessage,
  PendingQuestionState,
} from "../src/components/chat/types";
import type { UiSession } from "../src/types/ui";

type ComposerHarnessProps = {
  contextUsage: AgentContextUsageSummary | null;
  inputValue: string;
  isStreaming: boolean;
  runtimeConfigError: string;
  onInputChange: (value: string) => void;
  onSubmit: () => void;
  onComposerAction: () => void;
  onModelSelect: (model: SelectableModel) => void;
  onReasoningEffortSelect: (effort: ModelThinkingMode) => void;
  onCompact: () => void;
};

type MessageListHarnessProps = {
  editingUserMessageId: string | null;
  editingPrompt: string;
  onEditingPromptChange: (value: string) => void;
  onCancelEditingUserMessage: () => void;
  onSubmitEditedUserMessage: (messageId: string) => void;
  onCopyUserMessage: (messageId: string, text: string) => void;
  onStartEditingUserMessage: (
    message: Extract<ChatMessage, { role: "user" }>,
  ) => void;
};

type CapturedStream = {
  agentRunId: string;
  onMessage: (payload: AgentStreamPayload) => void;
  onError?: (error: Error) => void;
  onOpen?: () => void;
  close: ReturnType<typeof vi.fn>;
};

type ChatLifecycleCallbacks = {
  onAgentRunningChange?: (sessionId: string, isRunning: boolean) => void;
  onSessionCompleted?: (sessionId: string) => void;
};

type PendingQuestionHarnessProps = {
  pendingQuestion: PendingQuestionState;
  pendingQuestionError: string;
  onOptionToggle: (option: string) => void;
  onTextChange: (value: string) => void;
  onSubmit: () => void;
};

type AnswerAgentQuestionRequest = {
  sessionId?: string;
  questionId: string;
  answers?: string[];
  answerText?: string;
  autoContinueAfterResumeWait?: boolean;
};

type AgentQuestionResponse = {
  sessionId?: string;
  agentRunId?: string;
  turnId?: string;
};

type Deferred<T> = {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (reason?: unknown) => void;
};

const harness = vi.hoisted(() => ({
  composerProps: null as ComposerHarnessProps | null,
  messageListProps: null as MessageListHarnessProps | null,
  questionProps: null as PendingQuestionHarnessProps | null,
  streams: [] as CapturedStream[],
  nextRunNumber: 1,
  nextOperationNumber: 1,
  createSession: vi.fn(),
  sendAgentInput: vi.fn(),
  getAgentContextUsage: vi.fn(),
  answerAgentQuestion:
    vi.fn<
      (request: AnswerAgentQuestionRequest) => Promise<AgentQuestionResponse>
    >(),
  cancelAgentRun: vi.fn(),
}));

const runtimeConfig: AgentRuntimeConfig = {
  executionHost: "localUser",
  autoContinueAfterResumeWait: false,
  modelProviders: [],
  selectableModels: [],
  updatedAt: 1,
};

vi.mock("../src/lib/chatBridge", () => ({
  answerAgentQuestion: harness.answerAgentQuestion,
  cancelAgentRun: harness.cancelAgentRun,
  compactAgentContext: vi.fn(),
  createRuntimeOperationId: () => {
    const suffix = harness.nextOperationNumber.toString().padStart(12, "0");
    harness.nextOperationNumber += 1;
    return `00000000-0000-4000-8000-${suffix}`;
  },
  createSession: harness.createSession,
  getAgentContextUsage: harness.getAgentContextUsage,
  getAgentRuntimeConfig: vi.fn(async () => runtimeConfig),
  getAgentState: vi.fn(),
  getSession: vi.fn(),
  listAgentRuns: vi.fn(),
  openAgentStream: vi.fn(
    (
      agentRunId: string,
      onMessage: (payload: AgentStreamPayload) => void,
      onError?: (error: Error) => void,
      onOpen?: () => void,
    ) => {
      const close = vi.fn();
      harness.streams.push({
        agentRunId,
        onMessage,
        onError,
        onOpen,
        close,
      });
      return { close };
    },
  ),
  replayAgentRunStream: vi.fn(),
  sendAgentInput: harness.sendAgentInput,
  sendAgentSupplement: vi.fn(),
  setAgentRuntimeConfig: vi.fn(async () => runtimeConfig),
}));

vi.mock("../src/components/chat/useSessionHydration", () => ({
  useSessionHydration: () => ({
    isHydratingSession: false,
    hydrationStage: "",
  }),
}));

vi.mock("../src/components/chat/ChatComposer", () => ({
  ChatComposer: (props: ComposerHarnessProps) => {
    harness.composerProps = props;
    return <div data-testid="chat-composer" />;
  },
}));

vi.mock("../src/components/chat/VirtualMessageList", () => ({
  VirtualMessageList: (props: MessageListHarnessProps) => {
    harness.messageListProps = props;
    return <div data-testid="virtual-message-list" />;
  },
}));

vi.mock("../src/components/chat/ChatPendingPanels", () => ({
  PendingQuestionPanel: (props: PendingQuestionHarnessProps) => {
    harness.questionProps = props;
    return <div data-testid="pending-question" />;
  },
}));

vi.mock("../src/host/hostBridge", () => ({
  isNativeHostRuntime: () => true,
}));

import { ChatArea } from "../src/components/chat/ChatArea";
import { sessionViewCacheStore } from "../src/components/chat/chatRuntimeCore";
import { useChatViewStore } from "../src/components/chat/chatViewStore";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const originalWindowDescriptor = Object.getOwnPropertyDescriptor(
  globalThis,
  "window",
);

const session = (id: string): UiSession => ({
  id,
  title: `Session ${id}`,
  messageCount: 1,
  cwd: `D:\\Workspace\\${id}`,
  sessionKind: "main",
});

const terminalPayload = (
  sessionId: string,
  agentRunId: string,
  eventType:
    | "AgentRunCompleted"
    | "AgentRunFailed"
    | "AgentRunInterrupted" = "AgentRunCompleted",
  eventPayload: Record<string, unknown> = { doneReason: "finalized" },
): AgentStreamPayload => ({
  type: "session_event",
  agentRunId,
  event: {
    id: `${eventType}-${agentRunId}`,
    type: eventType,
    at: 100,
    sessionId,
    turnId: `turn-${agentRunId}`,
    taskId: agentRunId,
    parentTaskId: `turn-${agentRunId}`,
    visibility: "internal",
    payload: eventPayload,
  },
});

const textDeltaPayload = (
  sessionId: string,
  agentRunId: string,
  eventId: string,
  delta: string,
  visibility: "user" | "internal" = "user",
): AgentStreamPayload => ({
  type: "session_event",
  agentRunId,
  event: {
    id: eventId,
    type: "ModelTextDelta",
    at: 101,
    sessionId,
    turnId: `turn-${agentRunId}`,
    taskId: agentRunId,
    parentTaskId: `turn-${agentRunId}`,
    visibility,
    payload: { delta },
  },
});

const questionRequiredPayload = (
  sessionId: string,
  agentRunId: string,
): AgentStreamPayload => ({
  type: "session_event",
  agentRunId,
  event: {
    id: `QuestionRequired-${agentRunId}`,
    type: "QuestionRequired",
    at: 102,
    sessionId,
    turnId: `turn-${agentRunId}`,
    taskId: agentRunId,
    parentTaskId: `turn-${agentRunId}`,
    visibility: "user",
    payload: {
      message: "Choose a deployment region.",
      questionRequest: {
        id: "deployment-region",
        question: "Which region should be used?",
        options: ["Taipei", "Tokyo"],
        multiSelect: false,
        required: true,
      },
    },
  },
});

const modelRequestStartPayload = (
  sessionId: string,
  agentRunId: string,
): AgentStreamPayload => ({
  type: "session_event",
  agentRunId,
  event: {
    id: `ModelRequestStart-${agentRunId}`,
    type: "ModelRequestStart",
    at: 103,
    sessionId,
    turnId: `turn-${agentRunId}`,
    taskId: agentRunId,
    parentTaskId: `turn-${agentRunId}`,
    visibility: "user",
    payload: {
      purpose: "main",
      contextTokenEstimate: 999,
    },
  },
});

const createDeferred = <T,>(): Deferred<T> => {
  let resolvePromise: ((value: T) => void) | null = null;
  let rejectPromise: ((reason?: unknown) => void) | null = null;
  const promise = new Promise<T>((resolve, reject) => {
    resolvePromise = resolve;
    rejectPromise = reject;
  });
  return {
    promise,
    resolve: (value) => {
      if (!resolvePromise) {
        throw new Error("deferred resolve is unavailable");
      }
      resolvePromise(value);
    },
    reject: (reason) => {
      if (!rejectPromise) {
        throw new Error("deferred reject is unavailable");
      }
      rejectPromise(reason);
    },
  };
};

const getComposer = (): ComposerHarnessProps => {
  if (!harness.composerProps) {
    throw new Error("ChatComposer has not rendered");
  }
  return harness.composerProps;
};

const getMessageList = (): MessageListHarnessProps => {
  if (!harness.messageListProps) {
    throw new Error("VirtualMessageList has not rendered");
  }
  return harness.messageListProps;
};

const getUserMessage = (messageId: string): Extract<ChatMessage, { role: "user" }> => {
  const message = useChatViewStore.getState().messageById[messageId];
  if (message?.role !== "user") {
    throw new Error(`missing user message ${messageId}`);
  }
  return message;
};

const getQuestionPanel = (): PendingQuestionHarnessProps => {
  if (!harness.questionProps) {
    throw new Error("PendingQuestionPanel has not rendered");
  }
  return harness.questionProps;
};

const getStream = (index: number): CapturedStream => {
  const stream = harness.streams[index];
  if (!stream) {
    throw new Error(`missing captured stream ${index}`);
  }
  return stream;
};

const getAssistantTurn = (agentRunId: string): AssistantExecutionTurn => {
  for (const message of Object.values(
    useChatViewStore.getState().messageById,
  )) {
    if (
      message.role === "assistant" &&
      message.turn.agentRunId === agentRunId
    ) {
      return message.turn;
    }
  }
  throw new Error(`missing assistant turn for ${agentRunId}`);
};

const getAssistantMessageId = (agentRunId: string): string => {
  for (const [messageId, message] of Object.entries(
    useChatViewStore.getState().messageById,
  )) {
    if (
      message.role === "assistant" &&
      message.turn.agentRunId === agentRunId
    ) {
      return messageId;
    }
  }
  throw new Error(`missing assistant message for ${agentRunId}`);
};

const flushAsyncWork = async (): Promise<void> => {
  await Promise.resolve();
  await Promise.resolve();
};

const renderChat = async (
  currentSession: UiSession,
  callbacks: ChatLifecycleCallbacks = {},
  onRender?: ProfilerOnRenderCallback,
): Promise<ReactTestRenderer> => {
  let renderer: ReactTestRenderer | null = null;
  const chatArea = (
    <ChatArea
      currentSession={currentSession}
      currentSessionId={currentSession.id}
      workspaceName="Workspace"
      workspaceRoot="D:\\Workspace"
      onAgentRunningChange={callbacks.onAgentRunningChange}
      onSessionCompleted={callbacks.onSessionCompleted}
    />
  );
  await act(async () => {
    renderer = create(
      onRender ? (
        <Profiler id="ChatArea" onRender={onRender}>
          {chatArea}
        </Profiler>
      ) : (
        chatArea
      ),
    );
    await flushAsyncWork();
  });
  if (!renderer) {
    throw new Error("ChatArea did not render");
  }
  return renderer;
};

const renderEmptyChat = async (): Promise<ReactTestRenderer> => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(
      <ChatArea
        currentSession={null}
        currentSessionId={null}
        workspaceName="Workspace"
        workspaceRoot={"D:\\Workspace"}
      />,
    );
    await flushAsyncWork();
  });
  if (!renderer) {
    throw new Error("ChatArea did not render");
  }
  return renderer;
};

const submitPrompt = async (prompt: string): Promise<void> => {
  await act(async () => {
    getComposer().onInputChange(prompt);
  });
  await act(async () => {
    getComposer().onSubmit();
    await flushAsyncWork();
  });
};

const openStream = async (stream: CapturedStream): Promise<void> => {
  await act(async () => {
    stream.onOpen?.();
  });
};

const requireQuestion = async (stream: CapturedStream): Promise<void> => {
  await act(async () => {
    stream.onMessage(questionRequiredPayload("one", "run-1"));
    await flushAsyncWork();
  });
};

beforeEach(() => {
  harness.composerProps = null;
  harness.messageListProps = null;
  harness.questionProps = null;
  harness.streams.length = 0;
  harness.nextRunNumber = 1;
  harness.nextOperationNumber = 1;
  harness.createSession.mockReset();
  harness.sendAgentInput.mockReset();
  harness.answerAgentQuestion.mockReset();
  harness.cancelAgentRun.mockReset();
  harness.getAgentContextUsage.mockReset();
  harness.getAgentContextUsage.mockImplementation(async (sessionId: string) => ({
    sessionId,
    usedTokens: 20,
    maxContextTokens: 100,
    usedPercentage: 20,
    isCompacting: false,
    updatedAt: 1,
  }));
  harness.cancelAgentRun.mockResolvedValue({ cancelled: true });
  harness.createSession.mockResolvedValue({
    id: "created-session",
    title: "Created session",
    updatedAt: 1,
    sessionKind: "main",
    messageCount: 0,
  });
  harness.sendAgentInput.mockImplementation(
    async ({ sessionId }: { sessionId: string; message: string }) => {
      const runNumber = harness.nextRunNumber;
      harness.nextRunNumber += 1;
      return {
        sessionId,
        agentRunId: `run-${runNumber}`,
        turnId: `turn-${runNumber}`,
      };
    },
  );
  harness.answerAgentQuestion.mockResolvedValue({
    sessionId: "one",
    agentRunId: "run-question-answer",
    turnId: "turn-question-answer",
  });
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

test("a queued prompt is sent once after the active run reaches a terminal event", async () => {
  const currentSession = session("one");
  const renderer = await renderChat(currentSession);

  await submitPrompt("first prompt");
  const firstStream = getStream(0);
  await openStream(firstStream);
  await submitPrompt("queued prompt");

  await act(async () => {
    firstStream.onMessage(terminalPayload("one", "run-1"));
    await flushAsyncWork();
  });

  expect(harness.sendAgentInput).toHaveBeenCalledTimes(2);
  expect(harness.sendAgentInput.mock.calls.map(([request]) => request.message)).toEqual([
    "first prompt",
    "queued prompt",
  ]);
  const operationIds = harness.sendAgentInput.mock.calls.map(
    ([request]) => request.operationId,
  );
  expect(operationIds).toEqual([
    expect.stringMatching(/^[0-9a-f-]{36}$/),
    expect.stringMatching(/^[0-9a-f-]{36}$/),
  ]);
  expect(operationIds[1]).not.toBe(operationIds[0]);

  await act(async () => {
    renderer.update(
      <ChatArea
        currentSession={currentSession}
        currentSessionId="one"
        workspaceName="Workspace"
        workspaceRoot="D:\\Workspace"
      />,
    );
    await flushAsyncWork();
  });
  expect(harness.sendAgentInput).toHaveBeenCalledTimes(2);

  await act(async () => renderer.unmount());
});

test("parent rerenders preserve composer and message-list callback identities", async () => {
  const renderer = await renderChat(session("one"));
  await submitPrompt("render one conversation");
  const composerBefore = getComposer();
  const messageListBefore = getMessageList();

  await act(async () => {
    renderer.update(
      <ChatArea
        currentSession={session("one")}
        currentSessionId="one"
        workspaceName="Workspace rerendered"
        workspaceRoot="D:\\Workspace"
      />,
    );
    await flushAsyncWork();
  });

  const composerAfter = getComposer();
  const messageListAfter = getMessageList();
  expect(composerAfter.onSubmit).toBe(composerBefore.onSubmit);
  expect(composerAfter.onComposerAction).toBe(composerBefore.onComposerAction);
  expect(messageListAfter.onEditingPromptChange).toBe(
    messageListBefore.onEditingPromptChange,
  );
  expect(messageListAfter.onCancelEditingUserMessage).toBe(
    messageListBefore.onCancelEditingUserMessage,
  );
  expect(messageListAfter.onSubmitEditedUserMessage).toBe(
    messageListBefore.onSubmitEditedUserMessage,
  );
  expect(messageListAfter.onCopyUserMessage).toBe(
    messageListBefore.onCopyUserMessage,
  );
  expect(messageListAfter.onStartEditingUserMessage).toBe(
    messageListBefore.onStartEditingUserMessage,
  );

  await act(async () => renderer.unmount());
});

test("a queued prompt cannot cross into a newly selected session", async () => {
  const renderer = await renderChat(session("one"));

  await submitPrompt("first prompt");
  const firstStream = getStream(0);
  await openStream(firstStream);
  await submitPrompt("queued for one");

  await act(async () => {
    renderer.update(
      <ChatArea
        currentSession={session("two")}
        currentSessionId="two"
        workspaceName="Workspace"
        workspaceRoot="D:\\Workspace"
      />,
    );
  });
  await act(async () => {
    firstStream.onMessage(terminalPayload("one", "run-1"));
    await flushAsyncWork();
  });

  expect(harness.sendAgentInput).toHaveBeenCalledTimes(1);
  expect(harness.sendAgentInput).toHaveBeenLastCalledWith(
    expect.objectContaining({ sessionId: "one", message: "first prompt" }),
  );

  await act(async () => renderer.unmount());
});

test("stopping restores the queued prompt and cancels the active run once", async () => {
  const renderer = await renderChat(session("one"));

  await submitPrompt("first prompt");
  const firstStream = getStream(0);
  await openStream(firstStream);
  await submitPrompt("queued prompt");

  await act(async () => {
    getComposer().onComposerAction();
    await flushAsyncWork();
  });

  expect(getComposer().inputValue).toBe("queued prompt");
  expect(getComposer().isStreaming).toBe(false);
  expect(firstStream.close).toHaveBeenCalledTimes(1);
  expect(harness.cancelAgentRun).toHaveBeenCalledTimes(1);
  expect(harness.cancelAgentRun).toHaveBeenCalledWith({
    agentRunId: "run-1",
    sessionId: "one",
    reason: "user_interrupt",
  });

  await act(async () => renderer.unmount());
});

test("a late stream-open callback cannot revive a stopped run", async () => {
  const renderer = await renderChat(session("one"));

  await submitPrompt("first prompt");
  const firstStream = getStream(0);
  await openStream(firstStream);

  await act(async () => {
    getComposer().onComposerAction();
    await flushAsyncWork();
  });
  expect(getComposer().isStreaming).toBe(false);

  await openStream(firstStream);

  expect(firstStream.close).toHaveBeenCalledTimes(1);
  expect(getComposer().isStreaming).toBe(false);

  await act(async () => renderer.unmount());
});

test("the active stream owns connection-open and connection-error state", async () => {
  const onAgentRunningChange = vi.fn();
  const renderer = await renderChat(session("one"), {
    onAgentRunningChange,
  });

  await submitPrompt("first prompt");
  const firstStream = getStream(0);

  expect(getComposer().isStreaming).toBe(false);
  expect(onAgentRunningChange.mock.calls).toEqual([["one", true]]);

  await openStream(firstStream);
  expect(getComposer().isStreaming).toBe(true);

  await act(async () => {
    firstStream.onError?.(new Error("connection lost"));
    await flushAsyncWork();
  });

  expect(getComposer().isStreaming).toBe(false);
  expect(firstStream.close).toHaveBeenCalledTimes(1);
  expect(onAgentRunningChange.mock.calls).toEqual([
    ["one", true],
    ["one", false],
  ]);

  await openStream(firstStream);
  expect(getComposer().isStreaming).toBe(false);
  expect(onAgentRunningChange).toHaveBeenCalledTimes(2);

  await act(async () => renderer.unmount());
});

test("a terminal event closes and reports the active run exactly once", async () => {
  const onAgentRunningChange = vi.fn();
  const onSessionCompleted = vi.fn();
  const renderer = await renderChat(session("one"), {
    onAgentRunningChange,
    onSessionCompleted,
  });

  await submitPrompt("first prompt");
  const firstStream = getStream(0);
  await openStream(firstStream);
  const completed = terminalPayload("one", "run-1");

  await act(async () => {
    firstStream.onMessage(completed);
    await flushAsyncWork();
  });

  expect(getComposer().isStreaming).toBe(false);
  expect(firstStream.close).toHaveBeenCalledTimes(1);
  expect(onAgentRunningChange.mock.calls).toEqual([
    ["one", true],
    ["one", false],
  ]);
  expect(onSessionCompleted).toHaveBeenCalledTimes(1);
  expect(onSessionCompleted).toHaveBeenCalledWith("one");

  await act(async () => {
    firstStream.onMessage(completed);
    await flushAsyncWork();
  });

  expect(firstStream.close).toHaveBeenCalledTimes(1);
  expect(onAgentRunningChange).toHaveBeenCalledTimes(2);
  expect(onSessionCompleted).toHaveBeenCalledTimes(1);

  await act(async () => renderer.unmount());
});

test.each([
  {
    label: "failed",
    eventType: "AgentRunFailed" as const,
    eventPayload: { message: "provider failed" },
  },
  {
    label: "cancelled",
    eventType: "AgentRunInterrupted" as const,
    eventPayload: { reasonType: "cancelled" },
  },
  {
    label: "stopped",
    eventType: "AgentRunInterrupted" as const,
    eventPayload: { reasonType: "stopped" },
  },
])(
  "a $label terminal event closes and reports the active run exactly once",
  async ({ eventType, eventPayload }) => {
    const onAgentRunningChange = vi.fn();
    const onSessionCompleted = vi.fn();
    const renderer = await renderChat(session("one"), {
      onAgentRunningChange,
      onSessionCompleted,
    });

    await submitPrompt("first prompt");
    const firstStream = getStream(0);
    await openStream(firstStream);
    const terminal = terminalPayload(
      "one",
      "run-1",
      eventType,
      eventPayload,
    );

    await act(async () => {
      firstStream.onMessage(terminal);
      firstStream.onMessage(terminal);
      await flushAsyncWork();
    });

    expect(getComposer().isStreaming).toBe(false);
    expect(firstStream.close).toHaveBeenCalledTimes(1);
    expect(onAgentRunningChange.mock.calls).toEqual([
      ["one", true],
      ["one", false],
    ]);
    expect(onSessionCompleted).toHaveBeenCalledTimes(1);
    expect(onSessionCompleted).toHaveBeenCalledWith("one");

    await act(async () => renderer.unmount());
  },
);

test("a terminal event for another agent run fails closed without completing the session", async () => {
  const onAgentRunningChange = vi.fn();
  const onSessionCompleted = vi.fn();
  const renderer = await renderChat(session("one"), {
    onAgentRunningChange,
    onSessionCompleted,
  });

  await submitPrompt("first prompt");
  const firstStream = getStream(0);
  await openStream(firstStream);

  await act(async () => {
    firstStream.onMessage(terminalPayload("one", "foreign-run"));
    await flushAsyncWork();
  });

  expect(getComposer().isStreaming).toBe(false);
  expect(firstStream.close).toHaveBeenCalledTimes(1);
  expect(onAgentRunningChange.mock.calls).toEqual([
    ["one", true],
    ["one", false],
  ]);
  expect(onSessionCompleted).not.toHaveBeenCalled();

  await act(async () => renderer.unmount());
});

test("payloads arriving after a terminal event cannot mutate the completed turn", async () => {
  const onAgentRunningChange = vi.fn();
  const onSessionCompleted = vi.fn();
  const renderer = await renderChat(session("one"), {
    onAgentRunningChange,
    onSessionCompleted,
  });

  await submitPrompt("first prompt");
  const firstStream = getStream(0);
  await openStream(firstStream);

  await act(async () => {
    firstStream.onMessage(terminalPayload("one", "run-1"));
    await flushAsyncWork();
  });
  const completedTurn = getAssistantTurn("run-1");

  await act(async () => {
    firstStream.onMessage(
      textDeltaPayload("one", "run-1", "late-delta", "too late"),
    );
    await flushAsyncWork();
  });

  expect(getAssistantTurn("run-1")).toEqual(completedTurn);
  expect(firstStream.close).toHaveBeenCalledTimes(1);
  expect(onAgentRunningChange).toHaveBeenCalledTimes(2);
  expect(onSessionCompleted).toHaveBeenCalledTimes(1);

  await act(async () => renderer.unmount());
});

test("the visible session view is persisted once after the debounce window", async () => {
  const writeSpy = vi.spyOn(sessionViewCacheStore, "write");
  const renderer = await renderChat(session("one"));

  await submitPrompt("first prompt");
  expect(writeSpy).not.toHaveBeenCalled();

  await act(async () => {
    vi.advanceTimersByTime(499);
    await flushAsyncWork();
  });
  expect(writeSpy).not.toHaveBeenCalled();

  await act(async () => {
    vi.advanceTimersByTime(1);
    await flushAsyncWork();
  });

  expect(writeSpy).toHaveBeenCalledTimes(1);
  expect(sessionViewCacheStore.get("one")?.snapshot.activeReplay).toEqual({
    messageId: getAssistantMessageId("run-1"),
    agentRunId: "run-1",
  });

  writeSpy.mockRestore();
  await act(async () => renderer.unmount());
});

test("updates inside the debounce window persist only the latest completed view", async () => {
  const writeSpy = vi.spyOn(sessionViewCacheStore, "write");
  const renderer = await renderChat(session("one"));

  await submitPrompt("first prompt");
  const firstStream = getStream(0);
  await openStream(firstStream);

  await act(async () => {
    firstStream.onMessage(
      textDeltaPayload("one", "run-1", "answer-delta", "final answer"),
    );
    firstStream.onMessage(terminalPayload("one", "run-1"));
    await flushAsyncWork();
  });

  await act(async () => {
    vi.advanceTimersByTime(500);
    await flushAsyncWork();
  });

  const cached = sessionViewCacheStore.get("one");
  expect(writeSpy).toHaveBeenCalledTimes(1);
  expect(cached?.snapshot.activeReplay).toBeNull();
  expect(
    cached?.snapshot.messages.find((message) => message.role === "assistant")
      ?.turn.finalAnswer,
  ).toBe("final answer");

  writeSpy.mockRestore();
  await act(async () => renderer.unmount());
});

test("a pending cache write remains owned by its visible session", async () => {
  const renderer = await renderChat(session("one"));

  await submitPrompt("first prompt");
  await act(async () => {
    renderer.update(
      <ChatArea
        currentSession={session("two")}
        currentSessionId="two"
        workspaceName="Workspace"
        workspaceRoot="D:\\Workspace"
      />,
    );
    await flushAsyncWork();
  });
  await act(async () => {
    vi.advanceTimersByTime(500);
    await flushAsyncWork();
  });

  expect(sessionViewCacheStore.get("one")?.sessionId).toBe("one");
  expect(sessionViewCacheStore.get("two")).toBeNull();

  await act(async () => renderer.unmount());
});

test("unmounting cancels a pending session view cache write", async () => {
  const renderer = await renderChat(session("one"));

  await submitPrompt("first prompt");
  await act(async () => renderer.unmount());
  await act(async () => {
    vi.advanceTimersByTime(500);
    await flushAsyncWork();
  });

  expect(sessionViewCacheStore.size()).toBe(0);
});

test("late callbacks from an old connection cannot disturb its replacement", async () => {
  const onAgentRunningChange = vi.fn();
  const onSessionCompleted = vi.fn();
  const renderer = await renderChat(session("one"), {
    onAgentRunningChange,
    onSessionCompleted,
  });

  await submitPrompt("first prompt");
  const firstStream = getStream(0);
  await openStream(firstStream);
  await submitPrompt("second prompt");

  await act(async () => {
    firstStream.onMessage(terminalPayload("one", "run-1"));
    await flushAsyncWork();
  });
  const secondStream = getStream(1);
  await openStream(secondStream);

  expect(getComposer().isStreaming).toBe(true);
  expect(onAgentRunningChange.mock.calls).toEqual([
    ["one", true],
    ["one", false],
    ["one", true],
  ]);
  expect(onSessionCompleted).toHaveBeenCalledTimes(1);

  await act(async () => {
    firstStream.onOpen?.();
    firstStream.onError?.(new Error("late old connection error"));
    firstStream.onMessage({
      type: "error",
      message: "late old payload",
    });
    await flushAsyncWork();
  });

  expect(getComposer().isStreaming).toBe(true);
  expect(secondStream.close).not.toHaveBeenCalled();
  expect(onAgentRunningChange).toHaveBeenCalledTimes(3);
  expect(onSessionCompleted).toHaveBeenCalledTimes(1);

  await act(async () => {
    secondStream.onMessage(terminalPayload("one", "run-2"));
    await flushAsyncWork();
  });
  expect(getComposer().isStreaming).toBe(false);
  expect(secondStream.close).toHaveBeenCalledTimes(1);
  expect(onAgentRunningChange.mock.calls.at(-1)).toEqual(["one", false]);
  expect(onSessionCompleted).toHaveBeenCalledTimes(2);

  await act(async () => renderer.unmount());
});

test("submitting a required answer starts exactly one continuation stream", async () => {
  const renderer = await renderChat(session("one"));

  await submitPrompt("first prompt");
  const firstStream = getStream(0);
  await openStream(firstStream);
  await requireQuestion(firstStream);

  await act(async () => {
    getQuestionPanel().onOptionToggle("Taipei");
  });
  await act(async () => {
    getQuestionPanel().onSubmit();
    await flushAsyncWork();
  });

  expect(harness.answerAgentQuestion).toHaveBeenCalledTimes(1);
  expect(harness.answerAgentQuestion).toHaveBeenCalledWith({
    sessionId: "one",
    questionId: "deployment-region",
    answers: ["Taipei"],
    answerText: undefined,
    autoContinueAfterResumeWait: undefined,
  });
  expect(firstStream.close).toHaveBeenCalledTimes(1);
  expect(getStream(1).agentRunId).toBe("run-question-answer");
  expect(
    renderer.root.findAllByProps({ "data-testid": "pending-question" }),
  ).toHaveLength(0);

  await act(async () => renderer.unmount());
});

test("a failed answer remains selected and can be retried", async () => {
  harness.answerAgentQuestion.mockRejectedValueOnce(
    new Error("answer request failed"),
  );
  const renderer = await renderChat(session("one"));

  await submitPrompt("first prompt");
  const firstStream = getStream(0);
  await openStream(firstStream);
  await requireQuestion(firstStream);

  await act(async () => {
    getQuestionPanel().onOptionToggle("Tokyo");
  });
  await act(async () => {
    getQuestionPanel().onSubmit();
    await flushAsyncWork();
  });

  expect(getQuestionPanel().pendingQuestion.selectedOptions).toEqual(["Tokyo"]);
  expect(getQuestionPanel().pendingQuestion.submitting).toBe(false);
  expect(
    renderer.root.findAllByProps({ "data-testid": "pending-question" }),
  ).toHaveLength(1);

  await act(async () => {
    getQuestionPanel().onSubmit();
    await flushAsyncWork();
  });

  expect(harness.answerAgentQuestion).toHaveBeenCalledTimes(2);
  expect(getStream(1).agentRunId).toBe("run-question-answer");
  expect(
    renderer.root.findAllByProps({ "data-testid": "pending-question" }),
  ).toHaveLength(0);

  await act(async () => renderer.unmount());
});

test("rapid duplicate answer submissions start only one request", async () => {
  const pendingAnswer = createDeferred<AgentQuestionResponse>();
  harness.answerAgentQuestion.mockReturnValue(pendingAnswer.promise);
  const renderer = await renderChat(session("one"));

  await submitPrompt("first prompt");
  const firstStream = getStream(0);
  await openStream(firstStream);
  await requireQuestion(firstStream);
  await act(async () => {
    getQuestionPanel().onOptionToggle("Taipei");
  });

  const submit = getQuestionPanel().onSubmit;
  await act(async () => {
    submit();
    submit();
    await flushAsyncWork();
  });

  expect(harness.answerAgentQuestion).toHaveBeenCalledTimes(1);

  await act(async () => {
    pendingAnswer.resolve({
      sessionId: "one",
      agentRunId: "run-question-answer",
      turnId: "turn-question-answer",
    });
    await flushAsyncWork();
  });
  await act(async () => renderer.unmount());
});

test("an answer response arriving after a session switch cannot start an old-session stream", async () => {
  const pendingAnswer = createDeferred<AgentQuestionResponse>();
  harness.answerAgentQuestion.mockReturnValue(pendingAnswer.promise);
  const renderer = await renderChat(session("one"));

  await submitPrompt("first prompt");
  const firstStream = getStream(0);
  await openStream(firstStream);
  await requireQuestion(firstStream);
  await act(async () => {
    getQuestionPanel().onOptionToggle("Taipei");
  });
  await act(async () => {
    getQuestionPanel().onSubmit();
    await flushAsyncWork();
  });

  await act(async () => {
    renderer.update(
      <ChatArea
        currentSession={session("two")}
        currentSessionId="two"
        workspaceName="Workspace"
        workspaceRoot="D:\\Workspace"
      />,
    );
    await flushAsyncWork();
  });
  const streamCountAfterSwitch = harness.streams.length;

  await act(async () => {
    pendingAnswer.resolve({
      sessionId: "one",
      agentRunId: "late-old-session-run",
      turnId: "late-old-session-turn",
    });
    await flushAsyncWork();
  });

  expect(harness.streams).toHaveLength(streamCountAfterSwitch);
  expect(
    harness.streams.some(
      (stream) => stream.agentRunId === "late-old-session-run",
    ),
  ).toBe(false);

  await act(async () => renderer.unmount());
});

test("an accepted prompt atomically adopts durable message IDs before stream events arrive", async () => {
  const renderer = await renderChat(session("one"));

  await submitPrompt("first prompt");

  const stateAfterAcceptance = useChatViewStore.getState();
  expect(stateAfterAcceptance.messageIds).toEqual([
    "msg:user:turn-1",
    "msg:assistant:turn-1",
  ]);
  expect(stateAfterAcceptance.messageById["msg:user:turn-1"]).toMatchObject({
    role: "user",
    text: "first prompt",
  });
  expect(
    stateAfterAcceptance.messageById["msg:assistant:turn-1"],
  ).toMatchObject({
    role: "assistant",
  });
  expect(
    stateAfterAcceptance.messageIds.some(
      (messageId) =>
        messageId.startsWith("user-") || messageId.startsWith("assistant-"),
    ),
  ).toBe(false);

  const stream = getStream(0);
  await openStream(stream);
  await act(async () => {
    stream.onMessage(
      textDeltaPayload("one", "run-1", "durable-answer", "durable text"),
    );
    stream.onMessage(terminalPayload("one", "run-1"));
    await flushAsyncWork();
  });

  expect(
    useChatViewStore.getState().messageById["msg:assistant:turn-1"],
  ).toMatchObject({
    role: "assistant",
    turn: {
      agentRunId: "run-1",
      finalAnswer: "durable text",
    },
  });

  await act(async () => renderer.unmount());
});

test("editing the durable tail submits an exact rewrite transaction", async () => {
  const renderer = await renderChat(session("one"));

  await submitPrompt("original prompt");
  const firstStream = getStream(0);
  await openStream(firstStream);
  await act(async () => {
    firstStream.onMessage(
      textDeltaPayload("one", "run-1", "initial-answer", "original answer"),
    );
    firstStream.onMessage(terminalPayload("one", "run-1"));
    await flushAsyncWork();
  });

  const originalUserMessage = getUserMessage("msg:user:turn-1");
  await act(async () => {
    getMessageList().onStartEditingUserMessage(originalUserMessage);
    await flushAsyncWork();
  });
  expect(getMessageList().editingUserMessageId).toBe(originalUserMessage.id);
  await act(async () => {
    getMessageList().onEditingPromptChange("rewritten prompt");
    await flushAsyncWork();
  });
  expect(getMessageList().editingPrompt).toBe("rewritten prompt");
  await act(async () => {
    getMessageList().onSubmitEditedUserMessage(originalUserMessage.id);
    await flushAsyncWork();
  });

  expect(harness.sendAgentInput).toHaveBeenCalledTimes(2);
  expect(harness.sendAgentInput).toHaveBeenLastCalledWith({
    operationId: expect.stringMatching(/^[0-9a-f-]{36}$/),
    sessionId: "one",
    message: "rewritten prompt",
    preferredLocale: "zh-CN",
    autoContinueAfterResumeWait: undefined,
    tailPolicy: "rewriteLastUser",
    rewriteTargetMessageId: "msg:user:turn-1",
    rewriteExpectedTailMessageId: "msg:assistant:turn-1",
  });
  expect(useChatViewStore.getState().messageIds).toEqual([
    "msg:user:turn-2",
    "msg:assistant:turn-2",
  ]);
  expect(getStream(1).agentRunId).toBe("run-2");

  await act(async () => renderer.unmount());
});

test("a rejected rewrite restores the exact transcript and editable draft", async () => {
  const renderer = await renderChat(session("one"));

  await submitPrompt("original prompt");
  const firstStream = getStream(0);
  await openStream(firstStream);
  await act(async () => {
    firstStream.onMessage(
      textDeltaPayload("one", "run-1", "initial-answer", "original answer"),
    );
    firstStream.onMessage(terminalPayload("one", "run-1"));
    await flushAsyncWork();
  });
  const originalState = useChatViewStore.getState();
  const originalMessages = originalState.messageIds.map(
    (messageId) => originalState.messageById[messageId],
  );
  const originalUserMessage = getUserMessage("msg:user:turn-1");

  await act(async () => {
    getMessageList().onStartEditingUserMessage(originalUserMessage);
    await flushAsyncWork();
  });
  expect(getMessageList().editingUserMessageId).toBe(originalUserMessage.id);
  await act(async () => {
    getMessageList().onEditingPromptChange("retry this rewrite");
    await flushAsyncWork();
  });
  expect(getMessageList().editingPrompt).toBe("retry this rewrite");
  harness.sendAgentInput.mockRejectedValueOnce(new Error("rewrite rejected"));
  await act(async () => {
    getMessageList().onSubmitEditedUserMessage(originalUserMessage.id);
    await flushAsyncWork();
  });

  const restoredState = useChatViewStore.getState();
  expect(restoredState.messageIds).toEqual(originalState.messageIds);
  expect(
    restoredState.messageIds.map(
      (messageId) => restoredState.messageById[messageId],
    ),
  ).toEqual(originalMessages);
  expect(getMessageList().editingUserMessageId).toBe(originalUserMessage.id);
  expect(getMessageList().editingPrompt).toBe("retry this rewrite");
  expect(getComposer().runtimeConfigError).toContain("rewrite rejected");
  expect(harness.streams).toHaveLength(1);

  await act(async () => renderer.unmount());
});

test("a prompt response arriving after a session switch cannot remap messages or start an old-session stream", async () => {
  const pendingResponse = createDeferred<AgentQuestionResponse>();
  harness.sendAgentInput.mockReturnValue(pendingResponse.promise);
  const renderer = await renderChat(session("one"));

  await submitPrompt("old-session prompt");
  const temporaryMessageIds = [
    ...useChatViewStore.getState().messageIds,
  ];
  expect(temporaryMessageIds).toHaveLength(2);
  expect(harness.streams).toHaveLength(0);

  await act(async () => {
    renderer.update(
      <ChatArea
        currentSession={session("two")}
        currentSessionId="two"
        workspaceName="Workspace"
        workspaceRoot="D:\\Workspace"
      />,
    );
    await flushAsyncWork();
  });

  await act(async () => {
    pendingResponse.resolve({
      sessionId: "one",
      agentRunId: "late-old-session-run",
      turnId: "late-old-session-turn",
    });
    await flushAsyncWork();
  });

  expect
    .soft(useChatViewStore.getState().messageIds)
    .toEqual(temporaryMessageIds);
  expect.soft(harness.streams).toHaveLength(0);

  await act(async () => renderer.unmount());
});

test("switching away and back cannot restore ownership to an older prompt response", async () => {
  const pendingResponse = createDeferred<AgentQuestionResponse>();
  harness.sendAgentInput.mockReturnValue(pendingResponse.promise);
  const renderer = await renderChat(session("one"));

  await submitPrompt("older prompt");
  const temporaryMessageIds = [
    ...useChatViewStore.getState().messageIds,
  ];

  await act(async () => {
    renderer.update(
      <ChatArea
        currentSession={session("two")}
        currentSessionId="two"
        workspaceName="Workspace"
        workspaceRoot="D:\\Workspace"
      />,
    );
    await flushAsyncWork();
  });
  await act(async () => {
    renderer.update(
      <ChatArea
        currentSession={session("one")}
        currentSessionId="one"
        workspaceName="Workspace"
        workspaceRoot="D:\\Workspace"
      />,
    );
    await flushAsyncWork();
  });

  await act(async () => {
    pendingResponse.resolve({
      sessionId: "one",
      agentRunId: "superseded-run",
      turnId: "superseded-turn",
    });
    await flushAsyncWork();
  });

  expect(useChatViewStore.getState().messageIds).toEqual(temporaryMessageIds);
  expect(harness.streams).toHaveLength(0);

  await act(async () => renderer.unmount());
});

test("a newly created session adopts ownership of its first prompt response", async () => {
  const renderer = await renderEmptyChat();

  await submitPrompt("create this session");

  expect(harness.createSession).toHaveBeenCalledTimes(1);
  expect(harness.createSession).toHaveBeenCalledWith(
    "create this session",
    "D:\\Workspace",
    expect.stringMatching(/^[0-9a-f-]{36}$/),
  );
  expect(harness.sendAgentInput).toHaveBeenCalledWith(
    expect.objectContaining({
      sessionId: "created-session",
      message: "create this session",
    }),
  );
  expect(useChatViewStore.getState().messageIds).toEqual([
    "msg:user:turn-1",
    "msg:assistant:turn-1",
  ]);
  expect(getStream(0).agentRunId).toBe("run-1");

  await act(async () => renderer.unmount());
});

test("duplicate stream event IDs are reduced exactly once", async () => {
  const renderer = await renderChat(session("one"));

  await submitPrompt("first prompt");
  const stream = getStream(0);
  await openStream(stream);
  const duplicateDelta = textDeltaPayload(
    "one",
    "run-1",
    "duplicate-delta",
    "only once",
  );

  await act(async () => {
    stream.onMessage(duplicateDelta);
    stream.onMessage(duplicateDelta);
    stream.onMessage(terminalPayload("one", "run-1"));
    await flushAsyncWork();
  });

  expect(getAssistantTurn("run-1").finalAnswer).toBe("only once");

  await act(async () => renderer.unmount());
});

test("assistant text deltas wait for one animation frame and commit together", async () => {
  const renderer = await renderChat(session("one"));
  const scheduledFrames: FrameRequestCallback[] = [];
  let nextFrameId = 1;
  const requestAnimationFrame = vi.mocked(window.requestAnimationFrame);
  requestAnimationFrame.mockImplementation((callback) => {
    scheduledFrames.push(callback);
    const frameId = nextFrameId;
    nextFrameId += 1;
    return frameId;
  });
  const runNextFrame = async (): Promise<void> => {
    const frame = scheduledFrames.shift();
    if (!frame) {
      throw new Error("expected a scheduled animation frame");
    }
    await act(async () => {
      frame(0);
      await flushAsyncWork();
    });
  };

  await act(async () => {
    getComposer().onInputChange("first prompt");
  });
  await act(async () => {
    getComposer().onSubmit();
  });
  await runNextFrame();
  await runNextFrame();
  const stream = getStream(0);
  await openStream(stream);

  await act(async () => {
    stream.onMessage(
      textDeltaPayload("one", "run-1", "batched-delta-1", "first "),
    );
    stream.onMessage(
      textDeltaPayload("one", "run-1", "batched-delta-2", "second"),
    );
    await flushAsyncWork();
  });

  expect(scheduledFrames).toHaveLength(1);
  expect(getAssistantTurn("run-1").finalAnswer).toBe("");

  await runNextFrame();

  const turn = getAssistantTurn("run-1");
  expect(turn.finalAnswer).toBe("first second");
  expect(turn.activity).toBeNull();

  await act(async () => renderer.unmount());
});

test("a pure assistant text delta updates the store without committing ChatArea", async () => {
  const renderPhases: Array<"mount" | "update" | "nested-update"> = [];
  const onRender: ProfilerOnRenderCallback = (_id, phase) => {
    renderPhases.push(phase);
  };
  const renderer = await renderChat(session("one"), {}, onRender);
  const scheduledFrames: FrameRequestCallback[] = [];
  let nextFrameId = 1;
  vi.mocked(window.requestAnimationFrame).mockImplementation((callback) => {
    scheduledFrames.push(callback);
    const frameId = nextFrameId;
    nextFrameId += 1;
    return frameId;
  });
  const runNextFrame = async (): Promise<void> => {
    const frame = scheduledFrames.shift();
    if (!frame) {
      throw new Error("expected a scheduled animation frame");
    }
    await act(async () => {
      frame(0);
      await flushAsyncWork();
    });
  };
  await act(async () => {
    getComposer().onInputChange("first prompt");
  });
  await act(async () => {
    getComposer().onSubmit();
  });
  await runNextFrame();
  await runNextFrame();
  const stream = getStream(0);
  await openStream(stream);
  const commitsBeforeDelta = renderPhases.length;

  await act(async () => {
    stream.onMessage(
      textDeltaPayload("one", "run-1", "profiled-delta", "store only"),
    );
    await flushAsyncWork();
  });
  expect(scheduledFrames).toHaveLength(1);
  await runNextFrame();

  expect(getAssistantTurn("run-1").finalAnswer).toBe("store only");
  expect(renderPhases).toHaveLength(commitsBeforeDelta);

  await act(async () => renderer.unmount());
});

test("internal stream events cannot append user-visible model text", async () => {
  const renderer = await renderChat(session("one"));

  await submitPrompt("first prompt");
  const stream = getStream(0);
  await openStream(stream);

  await act(async () => {
    stream.onMessage(
      textDeltaPayload(
        "one",
        "run-1",
        "internal-delta",
        "must stay hidden",
        "internal",
      ),
    );
    stream.onMessage(terminalPayload("one", "run-1"));
    await flushAsyncWork();
  });

  expect(getAssistantTurn("run-1").finalAnswer).toBe("");

  await act(async () => renderer.unmount());
});

test("model request boundaries refresh canonical context usage without starting a poll", async () => {
  const renderer = await renderChat(session("one"));

  await submitPrompt("first prompt");
  const stream = getStream(0);
  harness.getAgentContextUsage.mockClear();

  await act(async () => {
    stream.onMessage(modelRequestStartPayload("one", "run-1"));
    await flushAsyncWork();
  });

  expect(harness.getAgentContextUsage).toHaveBeenCalledTimes(1);
  expect(harness.getAgentContextUsage).toHaveBeenCalledWith("one");
  expect(getComposer().contextUsage).toEqual({
    sessionId: "one",
    usedTokens: 20,
    maxContextTokens: 100,
    usedPercentage: 20,
    isCompacting: false,
    updatedAt: 1,
  });

  await act(async () => {
    vi.advanceTimersByTime(60_000);
    await flushAsyncWork();
  });
  expect(harness.getAgentContextUsage).toHaveBeenCalledTimes(1);

  await act(async () => renderer.unmount());
});

test("unrelated ChatArea renders preserve memoized composer callbacks", async () => {
  const currentSession = session("one");
  const renderer = await renderChat(currentSession);
  const before = getComposer();

  await act(async () => {
    renderer.update(
      <ChatArea
        currentSession={currentSession}
        currentSessionId={currentSession.id}
        workspaceName="Workspace"
        workspaceRoot="D:\\Workspace"
      />,
    );
    await flushAsyncWork();
  });

  const after = getComposer();
  expect(after.onSubmit).toBe(before.onSubmit);
  expect(after.onComposerAction).toBe(before.onComposerAction);
  expect(after.onModelSelect).toBe(before.onModelSelect);
  expect(after.onReasoningEffortSelect).toBe(before.onReasoningEffortSelect);
  expect(after.onCompact).toBe(before.onCompact);

  await act(async () => renderer.unmount());
});

test("an unsupported stream payload fails closed and closes the active connection", async () => {
  const onAgentRunningChange = vi.fn();
  const renderer = await renderChat(session("one"), {
    onAgentRunningChange,
  });

  await submitPrompt("first prompt");
  const stream = getStream(0);
  await openStream(stream);

  await act(async () => {
    stream.onMessage({ type: "unsupported" } as unknown as AgentStreamPayload);
    await flushAsyncWork();
  });

  const turn = getAssistantTurn("run-1");
  const narrative = turn.chunks
    .filter((chunk) => chunk.kind === "narrative")
    .map((chunk) => chunk.text)
    .join("\n");
  expect(turn.isStreaming).toBe(false);
  expect(narrative).toContain(
    "协议错误：不支持的 stream payload type=unsupported。",
  );
  expect(stream.close).toHaveBeenCalledTimes(1);
  expect(onAgentRunningChange.mock.calls).toEqual([
    ["one", true],
    ["one", false],
  ]);

  await act(async () => renderer.unmount());
});
