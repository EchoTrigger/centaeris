import type { RefObject } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { AgentRuntimeConfig } from "../src/lib/chatBridge";
import type { SessionHydrationSnapshot } from "../src/components/chat/types";
import type { UiSession } from "../src/types/ui";

type ScrollContainer = {
  clientHeight: number;
  scrollHeight: number;
  scrollTop: number;
};

type VirtualListHarnessProps = {
  containerRef: RefObject<HTMLDivElement | null>;
  onContentSizeChange: () => void;
  onScroll: () => void;
};

type ComposerHarnessProps = {
  onInputChange: (value: string) => void;
  onSubmit: () => void;
};

const harness = vi.hoisted(() => ({
  composerProps: null as ComposerHarnessProps | null,
  frameCallbacks: new Map<number, FrameRequestCallback>(),
  nextFrameId: 1,
  scrollContainer: {
    clientHeight: 400,
    scrollHeight: 1_000,
    scrollTop: 600,
  } as ScrollContainer,
  virtualListProps: null as VirtualListHarnessProps | null,
}));

const runtimeConfig: AgentRuntimeConfig = {
  executionHost: "localUser",
  autoContinueAfterResumeWait: false,
  modelProviders: [],
  selectableModels: [],
  updatedAt: 1,
};

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

vi.mock("../src/lib/chatBridge", () => ({
  answerAgentQuestion: vi.fn(),
  cancelAgentRun: vi.fn(),
  compactAgentContext: vi.fn(),
  createSession: vi.fn(),
  getAgentContextUsage: vi.fn(),
  getAgentRuntimeConfig: vi.fn(async () => runtimeConfig),
  getAgentState: vi.fn(),
  getSession: vi.fn(),
  getSessionProjection: vi.fn(),
  listAgentRuns: vi.fn(),
  openAgentStream: vi.fn(() => ({ close: vi.fn() })),
  replayAgentRunStream: vi.fn(),
  sendAgentInput: vi.fn(() => new Promise<never>(() => {})),
  sendAgentSupplement: vi.fn(),
  setAgentRuntimeConfig: vi.fn(),
}));

vi.mock("../src/components/chat/chatRuntimeModel", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("../src/components/chat/chatRuntimeModel")>();
  return {
    ...actual,
    buildSessionHydrationSnapshot: vi.fn(async (sessionId: string) =>
      makeSnapshot(sessionId)
    ),
  };
});

vi.mock("../src/components/chat/ChatComposer", () => ({
  ChatComposer: (props: ComposerHarnessProps) => {
    harness.composerProps = props;
    return <div data-testid="chat-composer" />;
  },
}));

vi.mock("../src/components/chat/VirtualMessageList", () => ({
  VirtualMessageList: (props: VirtualListHarnessProps) => {
    harness.virtualListProps = props;
    props.containerRef.current = harness.scrollContainer as HTMLDivElement;
    return <div data-testid="virtual-message-list" />;
  },
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

const renderChatArea = async (sessionId: string): Promise<ReactTestRenderer> => {
  let renderer: ReactTestRenderer | null = null;
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
  return renderer!;
};

const getVirtualListProps = (): VirtualListHarnessProps => {
  if (!harness.virtualListProps) {
    throw new Error("VirtualMessageList has not rendered");
  }
  return harness.virtualListProps;
};

const getComposerProps = (): ComposerHarnessProps => {
  if (!harness.composerProps) {
    throw new Error("ChatComposer has not rendered");
  }
  return harness.composerProps;
};

const runAnimationFrames = () => {
  const callbacks = Array.from(harness.frameCallbacks.values());
  harness.frameCallbacks.clear();
  callbacks.forEach((callback) => callback(0));
};

const finishInitialScroll = async () => {
  await act(async () => runAnimationFrames());
  harness.frameCallbacks.clear();
};

beforeEach(() => {
  harness.composerProps = null;
  harness.frameCallbacks.clear();
  harness.nextFrameId = 1;
  harness.scrollContainer.clientHeight = 400;
  harness.scrollContainer.scrollHeight = 1_000;
  harness.scrollContainer.scrollTop = 600;
  harness.virtualListProps = null;
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
        const frameId = harness.nextFrameId;
        harness.nextFrameId += 1;
        harness.frameCallbacks.set(frameId, callback);
        return frameId;
      }),
      cancelAnimationFrame: vi.fn((frameId: number) => {
        harness.frameCallbacks.delete(frameId);
      }),
      setTimeout: globalThis.setTimeout.bind(globalThis),
      clearTimeout: globalThis.clearTimeout.bind(globalThis),
    },
  });
});

afterEach(() => {
  harness.frameCallbacks.clear();
  sessionViewCacheStore.clear();
  useChatViewStore.getState().clear();
  vi.useRealTimers();
  if (originalWindowDescriptor) {
    Object.defineProperty(globalThis, "window", originalWindowDescriptor);
  } else {
    Reflect.deleteProperty(globalThis, "window");
  }
});

test("a queued follow frame respects the user scrolling away", async () => {
  const renderer = await renderChatArea("a");
  await finishInitialScroll();
  const list = getVirtualListProps();

  await act(async () => list.onContentSizeChange());
  expect(harness.frameCallbacks).toHaveLength(1);

  harness.scrollContainer.scrollTop = 100;
  await act(async () => list.onScroll());
  expect(renderer.root.findAllByProps({ "aria-label": "回到最新" })).toHaveLength(1);

  await act(async () => runAnimationFrames());
  expect(harness.scrollContainer.scrollTop).toBe(100);

  await act(async () => renderer.unmount());
});

test("content changes coalesce while jump-to-latest resumes following", async () => {
  const renderer = await renderChatArea("a");
  await finishInitialScroll();
  const list = getVirtualListProps();
  harness.scrollContainer.scrollTop = 100;
  await act(async () => list.onScroll());

  const jumpButton = renderer.root.findByProps({ "aria-label": "回到最新" });
  await act(async () => {
    jumpButton.props.onClick();
    list.onContentSizeChange();
    list.onContentSizeChange();
  });

  expect(harness.frameCallbacks).toHaveLength(1);
  expect(renderer.root.findAllByProps({ "aria-label": "回到最新" })).toHaveLength(0);
  await act(async () => runAnimationFrames());
  expect(harness.scrollContainer.scrollTop).toBe(1_000);

  await act(async () => renderer.unmount());
});

test("switching sessions resumes following", async () => {
  const renderer = await renderChatArea("a");
  await finishInitialScroll();
  const list = getVirtualListProps();
  harness.scrollContainer.scrollTop = 100;
  await act(async () => list.onScroll());

  await act(async () => {
    renderer.update(
      <ChatArea
        currentSession={makeSession("b")}
        currentSessionId="b"
        workspaceName="Workspace"
        workspaceRoot="D:\\Workspace"
      />,
    );
  });

  expect(renderer.root.findAllByProps({ "aria-label": "回到最新" })).toHaveLength(0);
  expect(harness.frameCallbacks).toHaveLength(1);
  await act(async () => runAnimationFrames());
  expect(harness.scrollContainer.scrollTop).toBe(1_000);

  await act(async () => renderer.unmount());
});

test("direct submission resumes following before the new content is measured", async () => {
  const renderer = await renderChatArea("a");
  await finishInitialScroll();
  harness.scrollContainer.scrollTop = 100;
  await act(async () => getVirtualListProps().onScroll());

  await act(async () => getComposerProps().onInputChange("hello"));
  await act(async () => getComposerProps().onSubmit());

  expect(renderer.root.findAllByProps({ "aria-label": "回到最新" })).toHaveLength(0);

  await act(async () => renderer.unmount());
});
