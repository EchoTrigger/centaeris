import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import {
  useAssistantTurnUpdateQueue,
  type AssistantTurnUpdateQueue,
} from "../src/components/chat/useAssistantTurnUpdateQueue";
import type { ChatMessage } from "../src/components/chat/types";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const originalWindowDescriptor = Object.getOwnPropertyDescriptor(
  globalThis,
  "window",
);

const initialMessages = (): ChatMessage[] => [
  {
    id: "assistant-one",
    role: "assistant",
    turn: {
      id: "turn-one",
      chunks: [],
      finalAnswer: "",
      isStreaming: true,
    },
  },
];

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  if (originalWindowDescriptor) {
    Object.defineProperty(globalThis, "window", originalWindowDescriptor);
  } else {
    Reflect.deleteProperty(globalThis, "window");
  }
});

test("text deltas preserve order around a semantic turn update", async () => {
  const scheduledFrames: FrameRequestCallback[] = [];
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: {
      requestAnimationFrame: vi.fn((callback: FrameRequestCallback) => {
        scheduledFrames.push(callback);
        return scheduledFrames.length;
      }),
      cancelAnimationFrame: vi.fn(),
    },
  });
  const messagesRef = { current: initialMessages() };
  const messageIndexByIdRef = {
    current: new Map([["assistant-one", 0]]),
  };
  const flushRef = { current: () => {} };
  const commitMessagesToView = vi.fn((nextMessages: ChatMessage[]) => {
    messagesRef.current = nextMessages;
  });
  let queue: AssistantTurnUpdateQueue | null = null;

  const Harness = () => {
    queue = useAssistantTurnUpdateQueue({
      messagesRef,
      messageIndexByIdRef,
      commitMessagesToView,
      flushAssistantTurnUpdatesRef: flushRef,
    });
    return null;
  };
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<Harness />);
  });
  const getQueue = (): AssistantTurnUpdateQueue => {
    if (!queue) {
      throw new Error("assistant turn queue did not initialize");
    }
    return queue;
  };
  const activeQueue = getQueue();

  activeQueue.appendAssistantTextDelta("assistant-one", "before");
  activeQueue.updateAssistantTurn("assistant-one", (turn) => ({
    ...turn,
    finalAnswer: `${turn.finalAnswer}|semantic|`,
  }));
  activeQueue.appendAssistantTextDelta("assistant-one", "after");

  expect(scheduledFrames).toHaveLength(1);
  expect(messagesRef.current[0]?.role).toBe("assistant");
  expect(
    messagesRef.current[0]?.role === "assistant"
      ? messagesRef.current[0].turn.finalAnswer
      : null,
  ).toBe("");

  await act(async () => {
    scheduledFrames[0]?.(0);
  });

  const assistant = messagesRef.current[0];
  expect(assistant?.role).toBe("assistant");
  expect(assistant?.role === "assistant" ? assistant.turn.finalAnswer : null)
    .toBe("before|semantic|after");
  expect(commitMessagesToView).toHaveBeenCalledTimes(1);
  expect(commitMessagesToView).toHaveBeenCalledWith(
    messagesRef.current,
    expect.objectContaining({
      assistantMessages: [assistant],
      refreshMeta: true,
    }),
  );

  await act(async () => renderer?.unmount());
});

test("unmount cancels a pending assistant update frame without committing", async () => {
  const cancelAnimationFrame = vi.fn();
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: {
      requestAnimationFrame: vi.fn((_callback: FrameRequestCallback) => 73),
      cancelAnimationFrame,
    },
  });
  const messagesRef = { current: initialMessages() };
  const messageIndexByIdRef = {
    current: new Map([["assistant-one", 0]]),
  };
  const flushRef = { current: () => {} };
  const commitMessagesToView = vi.fn();
  let queue: AssistantTurnUpdateQueue | null = null;
  const Harness = () => {
    queue = useAssistantTurnUpdateQueue({
      messagesRef,
      messageIndexByIdRef,
      commitMessagesToView,
      flushAssistantTurnUpdatesRef: flushRef,
    });
    return null;
  };
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<Harness />);
  });
  const getQueue = (): AssistantTurnUpdateQueue => {
    if (!queue) {
      throw new Error("assistant turn queue did not initialize");
    }
    return queue;
  };

  getQueue().appendAssistantTextDelta("assistant-one", "never committed");
  await act(async () => renderer?.unmount());

  expect(cancelAnimationFrame).toHaveBeenCalledWith(73);
  expect(commitMessagesToView).not.toHaveBeenCalled();
});
