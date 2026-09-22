import { createRef } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { AgentResultStream } from "../src/components/chat/AgentResultStream";
import { useChatViewStore } from "../src/components/chat/chatViewStore";
import { VirtualMessageList } from "../src/components/chat/VirtualMessageList";
import type { AssistantExecutionTurn } from "../src/components/chat/types";

vi.mock("../src/components/chat/MarkdownContent", () => ({
  MarkdownContent: ({ text }: { text: string }) => <p>{text}</p>,
}));
vi.mock("@tanstack/react-virtual", () => ({ useVirtualizer: () => ({
  getTotalSize: () => 220, getVirtualItems: () => [{ index: 0, start: 0 }],
  measurementsCache: [{ index: 0, start: 0 }], measureElement() {}, resizeItem() {},
}) }));
const read = vi.hoisted(() => vi.fn());
vi.mock("../src/components/chat/transcriptContentRanges", () => ({ loadTranscriptContentRange: read }));
let renderer: ReactTestRenderer;
afterEach(() => { act(() => renderer?.unmount()); useChatViewStore.getState().clear(); vi.unstubAllGlobals(); });
globalThis.IS_REACT_ACT_ENVIRONMENT = true;

beforeEach(() => {
  read.mockReset();
  vi.stubGlobal("window", { matchMedia: () => ({ matches: true }) });
  // A normal browser has this API. Deliberately deliver no scroll/visibility
  // notifications: titles and summaries must already be present after opening.
  vi.stubGlobal("IntersectionObserver", class {
    observe() {}
    unobserve() {}
    disconnect() {}
  });
});

test("a completed turn renders every stage in order without waiting for scrolling", () => {
  const turn: AssistantExecutionTurn = {
    id: "viewport", agentRunId: "viewport", finalAnswer: "Final answer", isStreaming: false,
    chunks: Array.from({ length: 40 }, (_, i) => ({ id: `stage-${i}`, kind: "narrative" as const, text: `Stage ${i}` })),
  };
  act(() => { renderer = create(<AgentResultStream turn={turn} />, { createNodeMock: () => ({ closest: () => null }) }); });
  const expected = [...Array.from({ length: 40 }, (_, i) => `Stage ${i}`), "Final answer"];
  expect(renderer.root.findAllByType("p").map(node => node.children.join(""))).toEqual(expected);
  expect(renderer.root.findAllByType("p").map(node => node.children.join(""))).toEqual(expected);
});


test("projected history renders every loaded process row without gaps", () => {
  useChatViewStore.getState().replaceMessages([
    ...Array.from({ length: 30 }, (_, i) => ({ id: `history-${i}`, role: "assistant" as const, turn: {
      id: `history-${i}`, chunks: [{ id: `stage-${i}`, kind: "narrative" as const, text: `History stage ${i}` }], finalAnswer: "", isStreaming: false,
    } })),
    { id: "final", role: "assistant", turn: { id: "final", chunks: [], finalAnswer: "History final", isStreaming: false } },
  ]);
  const noop = () => {};
  act(() => { renderer = create(<VirtualMessageList containerRef={createRef<HTMLDivElement>()}
    editingUserMessageId={null} editingPrompt="" copiedUserMessageId={null} latestUserMessageId={null} editableUserMessageId={null}
    onScroll={noop} onContentSizeChange={noop} onEditingPromptChange={noop} onCancelEditingUserMessage={noop}
    onSubmitEditedUserMessage={noop} onCopyUserMessage={noop} onStartEditingUserMessage={noop}
  />, { createNodeMock: () => ({ closest: () => null }) }); });
  expect(renderer.root.findAllByType("p").map(node => node.children.join(""))).toEqual([
    ...Array.from({ length: 30 }, (_, i) => `History stage ${i}`), "History final",
  ]);
});


test("live stages update without visibility notifications and final keeps the process visible", () => {
  const turn: AssistantExecutionTurn = { id: "live-viewport", agentRunId: "live-viewport", isStreaming: true, finalAnswer: "",
    chunks: [{ id: "live-stage", kind: "narrative", text: "Early stage" }],
  };
  act(() => { renderer = create(<AgentResultStream turn={turn} />, { createNodeMock: () => ({ closest: () => null }) }); });
  const updated: AssistantExecutionTurn = { ...turn, chunks: [{ id: "live-stage", kind: "narrative", text: "Latest stage" }] };
  act(() => renderer.update(<AgentResultStream turn={updated} />));
  expect(renderer.root.findByType("p").children).toEqual(["Latest stage"]);
  act(() => renderer.update(<AgentResultStream turn={{ ...updated, isStreaming: false, finalAnswer: "Done" }} />));
  expect(renderer.root.findAllByProps({ className: "workProgressSummary" })).toHaveLength(0);
  expect(renderer.root.findAllByType("p")[0].children).toEqual(["Latest stage"]);
  expect(renderer.root.findAllByType("p").at(-1)?.children).toEqual(["Done"]);
});


test("Tool cards immediately show complete titles while only the selected output is read", async () => {
  const turn: AssistantExecutionTurn = {
    id: "titles", agentRunId: "titles", isStreaming: false, finalAnswer: "Final answer",
    chunks: [
      { id: "reason", kind: "reasoning", text: "Existing reasoning summary", status: "done" },
      ...Array.from({ length: 40 }, (_, i) => ({
        id: `tool-${i}`, kind: "task" as const, task: {
          id: `tool-${i}`, title: "read", summary: "", status: "done" as const, provider: "tool" as const,
          displayTarget: `file-${i}.md`, operations: [{ callId: `call-${i}`, toolName: "read", status: "done" as const }],
          transcriptSessionId: "session", transcriptProjectionGeneration: "generation",
          transcriptContentRef: { refId: `output-${i}`, revision: "1", byteLength: "6" },
        },
      })),
    ],
  };
  read.mockResolvedValue({ content: "Output", startOffset: "0", endOffset: "6", hasMore: false });
  await act(async () => { renderer = create(<AgentResultStream turn={turn} />, { createNodeMock: () => ({ closest: () => null, scrollHeight: 100 }) }); });
  expect(renderer.root.findAllByProps({ className: "reasoningPreviewText" })).toHaveLength(0);
  expect(renderer.root.findAllByType("p").map(node => node.children.join(""))).toEqual(["Final answer"]);
  expect(read).not.toHaveBeenCalled();
  const members = renderer.root.findAllByProps({ className: "agent-operation-summary agent-tool-node-summary" });
  expect(members).toHaveLength(40);
  expect(members.map(node => node.findByProps({ className: "agent-tool-node-action is-inline-summary" }).children.join("")))
    .toEqual(Array.from({ length: 40 }, (_, i) => `Read file-${i}.md`));
  expect(renderer.root.findAllByType("pre")).toHaveLength(0);
  expect(read).not.toHaveBeenCalled();
  await act(async () => members[39].props.onClick({ preventDefault() {} }));
  expect(read).toHaveBeenCalledTimes(1);
  expect(read.mock.calls[0][0].reference.refId).toBe("output-39");
  expect(renderer.root.findByType("pre").children).toEqual(["Output"]);
});
