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
  await click(renderer.root.findByType("summary"));
  await click(renderer.root.findAllByType("summary")[1]);
  const output = () => renderer.root.findByProps({ className: "agent-tool-bash-output" }).children.join("");
  expect(output()).toBe("a".repeat(65536));
  await click(findText(renderer, "Next output"));
  expect(output()).toBe("b".repeat(65536));
  await click(findText(renderer, "Previous output"));
  expect(output()).toBe("a".repeat(65536));
  await act(async () => renderer.unmount());
});

test("defers complete Bash output until both activity levels are expanded", async () => {
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

  expect(JSON.stringify(renderer.toJSON())).toContain("Run the focused UI gate");
  expect(harness.readDesktopFilePreview).not.toHaveBeenCalled();

  await click(renderer.root.findByType("summary"));
  expect(harness.readDesktopFilePreview).not.toHaveBeenCalled();

  const operationSummary = renderer.root.findAllByType("summary")[1];
  if (!operationSummary) {
    throw new Error("Missing Bash operation summary");
  }
  await click(operationSummary);
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

  await click(renderer.root.findByType("summary"));
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

  await click(renderer.root.findByType("summary"));
  expect(harness.codePreviewRenders).toBe(0);

  const operationSummary = renderer.root.findAllByType("summary")[1];
  if (!operationSummary) {
    throw new Error("Missing edit operation summary");
  }
  await click(operationSummary);
  expect(harness.codePreviewRenders).toBe(1);
  expect(renderer.root.findByType("pre").children.join("")).toBe(diffPreview);

  await act(async () => renderer.unmount());
});

test("shows live status only when no running tool or final answer supersedes it", async () => {
  const baseTurn = makeTurn({
    isStreaming: true,
    activity: { kind: "thinking", label: "Thinking" },
    chunks: [{
      id: "process",
      kind: "narrative",
      text: "First process note",
    }],
  });
  const renderer = await renderStream({ turn: baseTurn });
  const initial = JSON.stringify(renderer.toJSON());
  expect(initial).toContain("First process note");
  expect(initial).toContain("Thinking");
  expect(initial.indexOf("First process note")).toBeLessThan(initial.indexOf("Thinking"));

  const runningTask = makeTask({
    status: "running",
    operations: [makeOperation({ status: "running", resultState: undefined })],
  });
  await act(async () => {
    renderer.update(<AgentResultStream turn={{
      ...baseTurn,
      chunks: [...baseTurn.chunks, { id: "running", kind: "task", task: runningTask }],
    }} />);
  });
  expect(JSON.stringify(renderer.toJSON())).not.toContain("Thinking");

  await act(async () => {
    renderer.update(<AgentResultStream turn={{
      ...baseTurn,
      finalAnswer: "Final answer",
    }} />);
  });
  const withFinal = JSON.stringify(renderer.toJSON());
  expect(withFinal).toContain("Final answer");
  expect(withFinal).not.toContain("Thinking");

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
  await click(renderer.root.findByType("button"));
  expect(onOpenAgentSession).toHaveBeenCalledWith(
    "session-child",
    "Investigate renderer",
  );

  await act(async () => renderer.unmount());
});

test("stream completion preserves the final Markdown subtree", async () => {
  const streamingTurn = makeTurn({
    finalAnswer: "Stable answer",
    isStreaming: true,
  });
  const renderer = await renderStream({ turn: streamingTurn });
  expect(harness.markdownMounts).toBe(1);
  expect(harness.markdownStreamingStates).toEqual([true]);

  await act(async () => {
    renderer.update(<AgentResultStream turn={{
      ...streamingTurn,
      isStreaming: false,
    }} />);
  });
  expect(harness.markdownMounts).toBe(1);
  expect(harness.markdownUnmounts).toBe(0);
  expect(harness.markdownStreamingStates).toEqual([true, false]);

  await act(async () => renderer.unmount());
  expect(harness.markdownUnmounts).toBe(1);
});

test("terminal text reuses the presentation mounted before the first live text", async () => {
  const streamingTurn = makeTurn({ isStreaming: true });
  const renderer = await renderStream({ turn: streamingTurn });
  expect(harness.presentationMounts).toBe(1);
  expect(harness.presentationStreamingStates).toEqual([true]);
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
  expect(harness.presentationStreamingStates).toEqual([true, false]);
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
