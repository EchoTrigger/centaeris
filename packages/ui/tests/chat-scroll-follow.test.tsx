vi.mock("../src/components/chat/ComposerPlane", async () => import("./helpers/composerPlaneTestDouble"));
import type { RefObject } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { TranscriptPageV1 } from "../src/lib/chatBridge";
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
  contentRef: RefObject<HTMLDivElement | null>;
  spacerRef: RefObject<HTMLDivElement | null>;
  onContentSizeChange: (size?: number, top?: number, userId?: string) => void;
  onScroll: () => void;
  onReadingIntent: () => void;
  onLoadOlder: () => void;
};

type ComposerHarnessProps = {
  onInputChange: (value: string) => void;
  onSubmit: () => void;
};

const harness = vi.hoisted(() => ({
  composerProps: null as ComposerHarnessProps | null,
  frameCallbacks: new Map<number, FrameRequestCallback>(),
  nextFrameId: 1,
  anchorAnimation: null as null | { progress(value: number): void; complete(): void },
  scrollContainer: {
    clientHeight: 400,
    scrollHeight: 1_000,
    scrollTop: 600,
  } as ScrollContainer,
  virtualListProps: null as VirtualListHarnessProps | null,
}));

// Follow measurements and Motion animation values have independent clocks.
// Keep the real ChatArea/controller and control only the library boundary.
vi.mock("motion", async (importOriginal) => {
  const actual = await importOriginal<typeof import("motion")>();
  return { ...actual, animate: (_from: number, _to: number, options: { onUpdate(value: number): void; onComplete(): void }) => {
    let active = true;
    harness.anchorAnimation = {
      progress(value) { if (active) options.onUpdate(value); },
      complete() { if (!active) return; options.onUpdate(1); active = false; options.onComplete(); },
    };
    return { stop() { active = false; } };
  } };
});

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
  replayCursorsByAgentRunId: {},
  pendingQuestionRequest: null,
  restoreMessageId: null,
  activeReplay: null,
});

vi.mock("../src/lib/chatBridge", () => ({
  TRANSCRIPT_PROJECTION_VERSION_V1: "transcript.projection.v1",
  answerAgentQuestion: vi.fn(),
  cancelAgentRun: vi.fn(),
  compactAgentContext: vi.fn(),
  createRuntimeOperationId: () => "00000000-0000-4000-8000-000000000001",
  createSession: vi.fn(),
  getAgentContextUsage: vi.fn(async () => { throw new Error("Context usage unavailable in fixture"); }),
  getAgentRuntimeConfig: vi.fn(async () => runtimeConfig),
  getAgentState: vi.fn(),
  getSession: vi.fn(),
  listAgentRuns: vi.fn(),
  openAgentStream: vi.fn(() => ({ close: vi.fn() })),
  getAgentRunLiveSnapshot: vi.fn(),
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
    props.containerRef.current = Object.assign(harness.scrollContainer, {
      getBoundingClientRect: () => ({ top: 0 }),
    }) as HTMLDivElement;
    props.contentRef.current = { getBoundingClientRect: () => ({
      top: -harness.scrollContainer.scrollTop, height: harness.scrollContainer.scrollHeight,
    }) } as HTMLDivElement;
    props.spacerRef.current = { style: { height: "0px" } } as HTMLDivElement;
    return <div data-testid="virtual-message-list" />;
  },
}));

vi.mock("../src/components/chat/ChatPendingPanels", () => ({
  PendingQuestionPanel: () => <div data-testid="pending-question" />,
}));

vi.mock("../src/host/hostBridge", () => ({
  isNativeHostRuntime: () => false,
}));

vi.mock("../src/components/chat/transcriptPaging", async (importOriginal) => ({
  ...await importOriginal<typeof import("../src/components/chat/transcriptPaging")>(),
  loadTranscriptPage: vi.fn(),
  catchUpTranscriptPatches: vi.fn(async () => {}),
}));
import { DesktopTranscriptView, loadTranscriptPage } from "../src/components/chat/transcriptPaging";
import { ChatArea } from "../src/components/chat/ChatArea";
import { sessionViewCacheStore } from "../src/components/chat/chatRuntimeCore";
import { buildSessionHydrationSnapshot } from "../src/components/chat/chatRuntimeModel";
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

const runAnimationFrames = (time = 0) => {
  const callbacks = Array.from(harness.frameCallbacks.values());
  harness.frameCallbacks.clear();
  callbacks.forEach((callback) => callback(time));
};

const finishInitialScroll = async () => {
  await act(async () => runAnimationFrames());
  harness.frameCallbacks.clear();
};

beforeEach(() => {
  harness.composerProps = null;
  harness.frameCallbacks.clear();
  harness.nextFrameId = 1;
  harness.anchorAnimation = null;
  harness.scrollContainer.clientHeight = 400;
  harness.scrollContainer.scrollHeight = 1_000;
  harness.scrollContainer.scrollTop = 600;
  harness.virtualListProps = null;
  sessionViewCacheStore.clear();
  useChatViewStore.getState().clear();
  vi.useFakeTimers();
  vi.stubGlobal("getComputedStyle", () => ({ paddingBottom: "0" }));
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
  vi.unstubAllGlobals();
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

  await act(async () => getVirtualListProps().onReadingIntent());
  harness.scrollContainer.scrollTop = 100;
  await act(async () => list.onScroll());
  expect(renderer.root.findAllByProps({ "aria-label": "Jump to latest" })).toHaveLength(1);

  await act(async () => runAnimationFrames());
  expect(harness.scrollContainer.scrollTop).toBe(100);

  await act(async () => renderer.unmount());
});

test("content changes coalesce while jump-to-latest resumes following", async () => {
  const renderer = await renderChatArea("a");
  await finishInitialScroll();
  const list = getVirtualListProps();
  await act(async () => getVirtualListProps().onReadingIntent());
  harness.scrollContainer.scrollTop = 100;
  await act(async () => list.onScroll());

  const jumpButton = renderer.root.findByProps({ "aria-label": "Jump to latest" });
  await act(async () => {
    jumpButton.props.onClick();
    list.onContentSizeChange();
    list.onContentSizeChange();
  });

  expect(harness.frameCallbacks).toHaveLength(1);
  expect(renderer.root.findAllByProps({ "aria-label": "Jump to latest" })).toHaveLength(0);
  await act(async () => runAnimationFrames());
  expect(harness.scrollContainer.scrollTop).toBe(600);

  await act(async () => renderer.unmount());
});

test("switching sessions resumes following", async () => {
  const renderer = await renderChatArea("a");
  await finishInitialScroll();
  const list = getVirtualListProps();
  await act(async () => getVirtualListProps().onReadingIntent());
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

  expect(renderer.root.findAllByProps({ "aria-label": "Jump to latest" })).toHaveLength(0);
  expect(harness.frameCallbacks).toHaveLength(1);
  await act(async () => runAnimationFrames());
  expect(harness.scrollContainer.scrollTop).toBe(600);

  await act(async () => renderer.unmount());
});

test("direct submission resumes following before the new content is measured", async () => {
  const renderer = await renderChatArea("a");
  await finishInitialScroll();
  await act(async () => getVirtualListProps().onReadingIntent());
  harness.scrollContainer.scrollTop = 100;
  await act(async () => getVirtualListProps().onScroll());

  await act(async () => getComposerProps().onInputChange("hello"));
  await act(async () => getComposerProps().onSubmit());

  expect(renderer.root.findAllByProps({ "aria-label": "Jump to latest" })).toHaveLength(0);

  await act(async () => renderer.unmount());
});


test("a submitted prompt anchors its measured user row instead of jumping to the bottom", async () => {
  const renderer = await renderChatArea("a");
  await finishInitialScroll();
  await act(async () => getVirtualListProps().onReadingIntent());
  harness.scrollContainer.scrollTop = 100;
  await act(async () => getVirtualListProps().onScroll());
  await act(async () => getComposerProps().onInputChange("hello"));
  await act(async () => getComposerProps().onSubmit());
  await act(async () => getVirtualListProps().onContentSizeChange(1000, 800, "new-user"));
  await act(async () => runAnimationFrames());
  expect(harness.scrollContainer.scrollTop).toBe(100);
  await act(async () => harness.anchorAnimation?.progress(0.5));
  expect(harness.scrollContainer.scrollTop).toBeGreaterThan(100);
  expect(harness.scrollContainer.scrollTop).toBeLessThan(700);
  await act(async () => harness.anchorAnimation?.complete());
  expect(harness.scrollContainer.scrollTop).toBe(700);
  expect(getVirtualListProps().spacerRef.current?.style.height).toBe("100px");
  await act(async () => renderer.unmount());
});


test("opening a detail cancels queued following until the user returns to latest", async () => {
  const renderer = await renderChatArea("detail");
  await finishInitialScroll();
  const list = getVirtualListProps();
  await act(async () => list.onContentSizeChange());
  await act(async () => list.onReadingIntent());
  harness.scrollContainer.scrollHeight += 240;
  await act(async () => runAnimationFrames());
  expect(harness.scrollContainer.scrollTop).toBe(600);
  const jump = renderer.root.findByProps({"aria-label":"Jump to latest"});
  await act(async () => jump.props.onClick());
  await act(async () => runAnimationFrames());
  expect(harness.scrollContainer.scrollTop).toBe(840);
  await act(async () => renderer.unmount());
});


test("a cold conversation keeps following after delayed hydration and row measurement", async () => {
  let resolve!: (value: SessionHydrationSnapshot) => void;
  const snapshot = new Promise<SessionHydrationSnapshot>(done => { resolve = done; });
  vi.mocked(buildSessionHydrationSnapshot).mockReturnValueOnce(snapshot);
  harness.scrollContainer.scrollTop = 0;
  const renderer = await renderChatArea("cold");
  try {
    await finishInitialScroll();
    await act(async () => { resolve(makeSnapshot("cold")); await snapshot; });
    const list = getVirtualListProps();
    await act(async () => list.onContentSizeChange());
    await act(async () => runAnimationFrames());
    harness.scrollContainer.scrollHeight += 500;
    await act(async () => { list.onContentSizeChange(); list.onScroll(); });
    await act(async () => runAnimationFrames());
    expect(harness.scrollContainer.scrollTop).toBe(harness.scrollContainer.scrollHeight - harness.scrollContainer.clientHeight);
    expect(renderer.root.findAllByProps({ "aria-label": "Jump to latest" })).toHaveLength(0);
  } finally { await act(async () => renderer.unmount()); }
});

test("upward reading intent during initial row growth cancels queued positioning", async () => {
  harness.scrollContainer.scrollTop = 0;
  const renderer = await renderChatArea("cold-reading");
  try {
    await finishInitialScroll();
    const list = getVirtualListProps();
    harness.scrollContainer.scrollHeight += 500;
    await act(async () => { list.onContentSizeChange(); list.onReadingIntent(); });
    await act(async () => getVirtualListProps().onReadingIntent());
  harness.scrollContainer.scrollTop = 100;
    await act(async () => { list.onScroll(); runAnimationFrames(); });
    expect(harness.scrollContainer.scrollTop).toBe(100);
    expect(renderer.root.findAllByProps({ "aria-label": "Jump to latest" })).toHaveLength(1);
  } finally { await act(async () => renderer.unmount()); }
});


test("an earlier-page response cannot overwrite scrolling performed while it was loading", async () => {
  const page = (id: string, sequence: string, olderCursor: string | null): TranscriptPageV1 => ({
    schema: "transcript.page.v1", sessionId: "paging", projectionVersion: "transcript.projection.v1",
    projectionGeneration: "generation", sourceHighWater: "2", olderCursor, hasOlder: olderCursor !== null,
    resumeCursors: [{ streamId: "session-jsonl.v1", cursor: "2" }],
    blocks: [{ blockId: id, blockRevision: "1", orderKey: { sourceSequence: sequence, ordinal: 0 },
      body: { kind: "userText", content: { inlineContent: id } } }],
  });
  const view = DesktopTranscriptView.open(page("newer", "2", "before-newer"));
  vi.mocked(buildSessionHydrationSnapshot).mockResolvedValueOnce({ ...makeSnapshot("paging"),
    messages: view.materializeMessages(false), transcriptView: view, transcriptHistoryMessageCount: 1 });
  let resolve!: (value: TranscriptPageV1) => void;
  vi.mocked(loadTranscriptPage).mockImplementationOnce(() => new Promise(done => { resolve = done; }));
  const renderer = await renderChatArea("paging");
  try {
    await finishInitialScroll();
    const list = getVirtualListProps();
    await act(async () => list.onReadingIntent());
    harness.scrollContainer.scrollTop = 0;
    vi.stubGlobal("requestAnimationFrame", window.requestAnimationFrame);
    await act(async () => list.onLoadOlder());
    // The reader moves before I/O finishes. The virtual list alone then shifts
    // that current offset by the inserted page height, preserving visible text.
    harness.scrollContainer.scrollTop = 150;
    await act(async () => resolve(page("older", "1", null)));
    expect(useChatViewStore.getState().messageIds).toEqual(["older", "newer"]);
    harness.scrollContainer.scrollHeight += 220;
    harness.scrollContainer.scrollTop += 220;
    await act(async () => { getVirtualListProps().onContentSizeChange(); runAnimationFrames(); });
    expect(harness.scrollContainer.scrollTop).toBe(370);
  } finally { await act(async () => renderer.unmount()); }
});

test("sent-turn row compensation does not show Jump to latest or release streaming follow", async () => {
  const renderer = await renderChatArea("sent-remeasurement");
  try {
    await finishInitialScroll();
    await act(async () => getComposerProps().onInputChange("hello"));
    await act(async () => getComposerProps().onSubmit());
    await act(async () => getVirtualListProps().onContentSizeChange(1000, 800, "sent-user"));
    await act(async () => runAnimationFrames());
    await act(async () => harness.anchorAnimation?.complete());
    expect(getVirtualListProps().spacerRef.current?.style.height).toBe("100px");

    harness.scrollContainer.scrollHeight -= 60;
    harness.scrollContainer.scrollTop -= 60;
    await act(async () => {
      getVirtualListProps().onContentSizeChange(940, 740, "sent-user");
      getVirtualListProps().onScroll();
    });
    expect(renderer.root.findAllByProps({ "aria-label": "Jump to latest" })).toHaveLength(0);
    await act(async () => runAnimationFrames());
    expect(harness.scrollContainer.scrollTop).toBe(640);

    harness.scrollContainer.scrollHeight = 1400;
    await act(async () => getVirtualListProps().onContentSizeChange(1400, 740, "sent-user"));
    await act(async () => runAnimationFrames());
    expect(harness.scrollContainer.scrollTop).toBe(1000);
    expect(getVirtualListProps().spacerRef.current?.style.height).toBe("0px");
    expect(renderer.root.findAllByProps({ "aria-label": "Jump to latest" })).toHaveLength(0);
  } finally {
    await act(async () => renderer.unmount());
  }
});
