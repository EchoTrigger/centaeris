import { useEffect, type ComponentProps } from "react";
import {
  act,
  create,
  type ReactTestInstance,
  type ReactTestRenderer,
} from "react-test-renderer";
import { beforeEach, expect, test, vi } from "vitest";
import type { DesktopFilePreviewReadResponse } from "../src/lib/workspaceBridge";
import type {
  TranscriptContentRangeV1,
} from "../src/lib/chatBridge";
import type {
  AssistantExecutionTurn,
  SubagentResult,
  TaskResult,
  ToolOperation,
} from "../src/components/chat/types";

const harness = vi.hoisted(() => ({
  markdownMounts: 0,
  markdownUnmounts: 0,
  markdownStreamingStates: [] as Array<boolean | undefined>,
  markdownRendersByText: new Map<string, number>(),
  presentationMounts: 0,
  presentationUnmounts: 0,
  presentationStreamingStates: [] as boolean[],
  codePreviewRenders: 0,
  loadTranscriptContentRange: vi.fn<(...args: unknown[]) => Promise<TranscriptContentRangeV1>>(),
  readDesktopFilePreview: vi.fn<
    (path: string) => Promise<DesktopFilePreviewReadResponse>
  >(),
}));

vi.mock("../src/lib/workspaceBridge", () => ({
  readDesktopFilePreview: harness.readDesktopFilePreview,
}));

vi.mock("../src/components/chat/transcriptContentRanges", () => ({
  loadTranscriptContentRange: harness.loadTranscriptContentRange,
  TRANSCRIPT_CONTENT_RANGE_BYTES: 65536,
}));

vi.mock("../src/useStreamPresentation", () => ({
  useStreamPresentation(text: string, live: boolean) {
    harness.presentationStreamingStates.push(live);
    useEffect(() => {
      harness.presentationMounts += 1;
      return () => {
        harness.presentationUnmounts += 1;
      };
    }, []);
    return text;
  },
}));

vi.mock("../src/components/chat/MarkdownContent", () => ({
  MarkdownContent({
    text,
    isStreaming,
  }: {
    text: string;
    isStreaming?: boolean;
  }) {
    harness.markdownStreamingStates.push(isStreaming);
    harness.markdownRendersByText.set(
      text,
      (harness.markdownRendersByText.get(text) ?? 0) + 1,
    );
    useEffect(() => {
      harness.markdownMounts += 1;
      return () => {
        harness.markdownUnmounts += 1;
      };
    }, []);
    return <div data-streaming={isStreaming ? "true" : "false"}>{text}</div>;
  },
}));

vi.mock("../src/components/CodePreview", () => ({
  default({ content }: { content: string }) {
    harness.codePreviewRenders += 1;
    return <pre>{content}</pre>;
  },
}));

import { useChatViewStore } from "../src/components/chat/chatViewStore";
import { AgentResultStream } from "../src/components/chat/AgentResultStream";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const makeOperation = (
  overrides: Partial<ToolOperation> = {},
): ToolOperation => ({
  callId: "call-one",
  toolName: "bash",
  kind: "command",
  status: "done",
  resultState: "successWithOutput",
  ...overrides,
});

const makeTask = (
  overrides: Partial<TaskResult> = {},
): TaskResult => ({
  id: "task-one",
  title: "bash",
  summary: "",
  status: "done",
  provider: "tool",
  operations: [makeOperation()],
  ...overrides,
});

const makeTurn = (
  overrides: Partial<AssistantExecutionTurn> = {},
): AssistantExecutionTurn => ({
  id: "turn-one",
  chunks: [],
  finalAnswer: "",
  isStreaming: false,
  ...overrides,
});

const renderStream = async (
  props: ComponentProps<typeof AgentResultStream>,
): Promise<ReactTestRenderer> => {
  const rendered = { current: null as ReactTestRenderer | null };
  await act(async () => {
    rendered.current = create(<AgentResultStream {...props} />);
  });
  if (!rendered.current) {
    throw new Error("Agent result stream did not render");
  }
  return rendered.current;
};

const click = async (node: ReactTestInstance): Promise<void> => {
  await act(async () => {
    node.props.onClick({ preventDefault: () => {} });
  });
};

const findText = (
  renderer: ReactTestRenderer,
  text: string,
): ReactTestInstance => {
  const match = renderer.root.findAll(
    (node) => node.children.some((child) => child === text),
  )[0];
  if (!match) {
    throw new Error(`Missing rendered text: ${text}`);
  }
  return match;
};

beforeEach(() => {
  useChatViewStore.getState().clear();
  harness.markdownMounts = 0;
  harness.markdownUnmounts = 0;
  harness.markdownStreamingStates.length = 0;
  harness.markdownRendersByText.clear();
  harness.presentationMounts = 0;
  harness.presentationUnmounts = 0;
  harness.presentationStreamingStates.length = 0;
  harness.codePreviewRenders = 0;
  harness.readDesktopFilePreview.mockReset();
  harness.loadTranscriptContentRange.mockReset();
});

test("paged tool output replaces retained text and can return to its previous range", async () => {
  harness.loadTranscriptContentRange.mockImplementation(async (_identity, offset) => {
    const start = Number(offset);
    return {
      schema: "transcript.content.range.v1", sessionId: "session-1",
      projectionVersion: "transcript.projection.v1", projectionGeneration: "generation-1",
      refId: "tool-output:call-1", revision: "2", byteLength: String(3 * 65536),
      startOffset: String(start), endOffset: String(start + 65536),
      content: (start === 0 ? "a" : "b").repeat(65536), hasMore: start + 65536 < 3 * 65536,
    };
  });
  const task = makeTask({
    transcriptContentRef: { refId: "tool-output:call-1", revision: "2", byteLength: String(3 * 65536) },
    transcriptSessionId: "session-1", transcriptProjectionGeneration: "generation-1",
    outputByteLength: 3 * 65536,
  });
  const renderer = await renderStream({ turn: makeTurn({ chunks: [{ id: "task", kind: "task", task }] }) });
  await click(renderer.root.findByProps({ className: "agent-operation-summary agent-tool-node-summary" }));
  const output = () => renderer.root.findByProps({ className: "agent-tool-bash-output" }).children.join("");
  expect(output()).toBe("a".repeat(65536));
  await click(findText(renderer, "Next output"));
  expect(output()).toBe("b".repeat(65536));
  await click(findText(renderer, "Previous output"));
  expect(output()).toBe("a".repeat(65536));
  await act(async () => renderer.unmount());
});

test("defers complete Bash output until its single tool is expanded", async () => {
  const prefix = "header\n";
  const result = "complete output";
  harness.readDesktopFilePreview.mockResolvedValue({
    root: "D:/workspace",
    path: "D:/spill.txt",
    name: "spill.txt",
    content: `${prefix}${result}`,
    byteLen: prefix.length + result.length,
    encoding: "utf-8",
    contentKind: "text",
  });
  const task = makeTask({
    normalizedInput: {
      command: "npm test",
      description: "Run the focused UI gate",
    },
    fullOutputPath: "D:/spill.txt",
    outputStartByte: new TextEncoder().encode(prefix).length,
    outputByteLength: new TextEncoder().encode(result).length,
  });
  const renderer = await renderStream({
    turn: makeTurn({ chunks: [{ id: "task-chunk", kind: "task", task }] }),
  });

  expect(JSON.stringify(renderer.toJSON())).toContain("npm test");
  expect(harness.readDesktopFilePreview).not.toHaveBeenCalled();

  await click(renderer.root.findByProps({ className: "agent-operation-summary agent-tool-node-summary" }));
  expect(harness.readDesktopFilePreview).toHaveBeenCalledOnce();
  expect(harness.readDesktopFilePreview).toHaveBeenCalledWith("D:/spill.txt");
  expect(JSON.stringify(renderer.toJSON())).toContain(result);

  await act(async () => renderer.unmount());
});

test("opens a detail-free file operation with its exact source range", async () => {
  const onOpenWorkspacePath = vi.fn();
  const task = makeTask({
    id: "read-task",
    title: "read",
    operations: [makeOperation({
      callId: "read-call",
      toolName: "read",
      kind: undefined,
      path: "src/App.tsx",
      startLine: 12,
      endLine: 24,
      resultState: "successWithoutOutput",
    })],
  });
  const renderer = await renderStream({
    turn: makeTurn({ chunks: [{ id: "read-chunk", kind: "task", task }] }),
    onOpenWorkspacePath,
  });

  await click(renderer.root.findByProps({ "aria-label": "Open src/App.tsx" }));
  expect(onOpenWorkspacePath).toHaveBeenCalledWith("src/App.tsx", {
    startLine: 12,
    endLine: 24,
    taskId: "read-task",
  });

  await act(async () => renderer.unmount());
});

test("does not mount a diff preview until its operation is expanded", async () => {
  const diffPreview = "--- src/App.tsx\n+++ src/App.tsx";
  const task = makeTask({
    id: "edit-task",
    title: "edit",
    operations: [makeOperation({
      callId: "edit-call",
      toolName: "edit",
      kind: undefined,
      path: "src/App.tsx",
      diffPreview,
    })],
  });
  const renderer = await renderStream({
    turn: makeTurn({ chunks: [{ id: "edit-chunk", kind: "task", task }] }),
  });
  expect(harness.codePreviewRenders).toBe(0);

  await click(renderer.root.findByProps({ className: "agent-operation-summary agent-tool-node-summary" }));
  expect(harness.codePreviewRenders).toBe(1);
  expect(renderer.root.findByType("pre").children.join("")).toBe(diffPreview);

  await act(async () => renderer.unmount());
});

test("live status follows the latest content, tracks a tool and disappears on completion", async () => {
  const turn = makeTurn({isStreaming:true, activity:{kind:"thinking",label:"Thinking"}, chunks:[{id:"note",kind:"narrative",text:"First process note"}]});
  useChatViewStore.getState().replaceMessages([{id:"message",role:"assistant",turn}]);
  const renderer = await renderStream({turn});
  const initial = JSON.stringify(renderer.toJSON());
  expect(initial.indexOf("Thinking")).toBeGreaterThan(initial.indexOf("First process note"));
  const running = {...turn, chunks:[...turn.chunks,{id:"task",kind:"task" as const,task:makeTask({status:"running"})}]};
  await act(async () => {
    useChatViewStore.getState().replaceMessages([{id:"message",role:"assistant",turn:running}]);
    renderer.update(<AgentResultStream turn={running}/>);
  });
  expect(renderer.root.findByProps({role:"status"}).children).toEqual(["Running a command…"]);
  const answering = {...running,finalAnswer:"Latest answer",finalAnswerConfirmed:true};
  await act(async () => {
    useChatViewStore.getState().replaceMessages([{id:"message",role:"assistant",turn:answering}]);
    renderer.update(<AgentResultStream turn={answering}/>);
  });
  const active = JSON.stringify(renderer.toJSON());
  expect(active.indexOf("Running a command…")).toBeGreaterThan(active.indexOf("Latest answer"));
  const done = {...turn,isStreaming:false,finalAnswer:"Final answer"};
  await act(async () => {
    useChatViewStore.getState().replaceMessages([{id:"message",role:"assistant",turn:done}]);
    renderer.update(<AgentResultStream turn={done}/>);
  });
  expect(renderer.root.findAllByProps({role:"status"})).toHaveLength(0);
  expect(JSON.stringify(renderer.toJSON())).toContain("Final answer");
  await act(async () => renderer.unmount());
});

test("opens a durable subagent session with its visible title", async () => {
  const onOpenAgentSession = vi.fn();
  const subagent = {
    id: "subagent-entry",
    subagentId: "agent-one",
    childSessionId: "session-child",
    title: "Fallback title",
    description: " Investigate renderer ",
    summary: "",
    status: "done",
  } satisfies SubagentResult;
  const renderer = await renderStream({
    turn: makeTurn({
      chunks: [{ id: "subagent-chunk", kind: "subagent", subagent }],
    }),
    onOpenAgentSession,
  });

  expect(findText(renderer, "Investigate renderer")).toBeDefined();
  await click(renderer.root.findAllByType("button").find((button) => button.props.className !== "workProgressSummary")!);
  expect(onOpenAgentSession).toHaveBeenCalledWith(
    "session-child",
    "Investigate renderer",
  );

  await act(async () => renderer.unmount());
});

test("completion displays the buffered final once without remounting it", async () => {
  const streamingTurn = makeTurn({
    finalAnswer: "Stable answer",
    isStreaming: true,
  });
  const renderer = await renderStream({ turn: streamingTurn });
  expect(harness.markdownMounts).toBe(0);
  expect(harness.markdownStreamingStates).toEqual([]);

  await act(async () => {
    renderer.update(<AgentResultStream turn={{
      ...streamingTurn,
      isStreaming: false,
    }} />);
  });
  expect(harness.markdownMounts).toBe(1);
  expect(harness.markdownUnmounts).toBe(0);
  expect(harness.markdownStreamingStates).toEqual([false]);

  await act(async () => renderer.unmount());
  expect(harness.markdownUnmounts).toBe(1);
});

test("terminal text reuses the presentation mounted before the first live text", async () => {
  const streamingTurn = makeTurn({ isStreaming: true });
  const renderer = await renderStream({ turn: streamingTurn });
  expect(harness.presentationMounts).toBe(1);
  expect(harness.presentationStreamingStates).toEqual([false]);
  expect(harness.markdownMounts).toBe(0);

  await act(async () => {
    renderer.update(<AgentResultStream turn={{
      ...streamingTurn,
      finalAnswer: "Terminal answer arrived with completion",
      isStreaming: false,
    }} />);
  });

  expect(harness.presentationMounts).toBe(1);
  expect(harness.presentationUnmounts).toBe(0);
  expect(harness.presentationStreamingStates).toEqual([false, false]);
  expect(harness.markdownMounts).toBe(1);

  await act(async () => renderer.unmount());
});

test("final answer deltas do not rerender an unchanged process transcript", async () => {
  const processChunk = {
    id: "process",
    kind: "narrative",
    text: "Stable process note",
  } as const;
  const initialTurn = makeTurn({
    chunks: [processChunk],
    finalAnswer: "Answer one",
    finalAnswerConfirmed: true,
    isStreaming: true,
  });
  const renderer = await renderStream({ turn: initialTurn });
  expect(harness.markdownRendersByText.get("Stable process note")).toBe(1);

  await act(async () => {
    renderer.update(<AgentResultStream turn={{
      ...initialTurn,
      finalAnswer: "Answer two",
    }} />);
  });
  expect(harness.markdownRendersByText.get("Stable process note")).toBe(1);

  await act(async () => renderer.unmount());
});

test("process updates do not rerender an unchanged final answer", async () => {
  const initialTurn = makeTurn({
    chunks: [{ id: "process", kind: "narrative", text: "Process one" }],
    finalAnswer: "Stable final answer",
    finalAnswerConfirmed: true,
    isStreaming: true,
  });
  const renderer = await renderStream({ turn: initialTurn });
  expect(harness.markdownRendersByText.get("Stable final answer")).toBe(1);

  await act(async () => {
    renderer.update(<AgentResultStream turn={{
      ...initialTurn,
      chunks: [{ id: "process-two", kind: "narrative", text: "Process two" }],
    }} />);
  });
  expect(harness.markdownRendersByText.get("Stable final answer")).toBe(1);

  await act(async () => renderer.unmount());
});


test("unclassified text stays buffered until the runtime confirms final", async () => {
  const turn = makeTurn({ finalAnswer: "Final response", isStreaming: true });
  const renderer = await renderStream({ turn });
  expect(JSON.stringify(renderer.toJSON())).not.toContain("Final response");
  expect(renderer.root.findAllByProps({ className: "workProgressSummary" })).toHaveLength(0);
  await act(async () => renderer.update(<AgentResultStream turn={{ ...turn, finalAnswerConfirmed: true }} />));
  expect(JSON.stringify(renderer.toJSON())).toContain("Final response");
  expect(renderer.root.findAllByProps({ className: "workProgressSummary" })).toHaveLength(0);
  expect(renderer.root.findAllByProps({ className: "agentAssistantAnswer answerMarkdownBlock" })).toHaveLength(1);
  expect(harness.markdownMounts).toBe(1);
  expect(harness.markdownStreamingStates).toEqual([false]);
  await act(async () => renderer.unmount());
});
