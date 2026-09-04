import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { beforeEach, expect, test, vi } from "vitest";
import type {
  AssistantExecutionTurn,
  ChatMessage,
  TaskResult,
  ToolOperation,
} from "../src/components/chat/types";

type AssistantChatMessage = Extract<ChatMessage, { role: "assistant" }>;

const harness = vi.hoisted(() => ({
  presentationCalls: [] as string[],
}));

vi.mock("../src/components/chat/toolActivityModel", async (importOriginal) => {
  const actual = await importOriginal<
    typeof import("../src/components/chat/toolActivityModel")
  >();
  return {
    ...actual,
    getToolActivityPresentation(operations: ToolOperation[]) {
      harness.presentationCalls.push(operations[0]?.callId ?? "missing");
      return actual.getToolActivityPresentation(operations);
    },
  };
});

import { AgentResultStream } from "../src/components/chat/AgentResultStream";
import { useChatViewStore } from "../src/components/chat/chatViewStore";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const makeTask = (id: string, path: string): TaskResult => ({
  id,
  title: "read",
  summary: "",
  status: "done",
  provider: "tool",
  operations: [{
    callId: `${id}-call`,
    toolName: "read",
    status: "done",
    resultState: "successWithoutOutput",
    path,
  }],
});

const makeTurn = (first: TaskResult, second: TaskResult): AssistantExecutionTurn => ({
  id: "turn-one",
  chunks: [
    { id: "first", kind: "task", task: first },
    { id: "separator", kind: "narrative", text: "Next stage" },
    { id: "second", kind: "task", task: second },
  ],
  finalAnswer: "",
  isStreaming: true,
});

const assistantMessage = (turn: AssistantExecutionTurn): AssistantChatMessage => ({
  id: "assistant-message",
  role: "assistant",
  turn,
});

beforeEach(() => {
  harness.presentationCalls.length = 0;
  useChatViewStore.getState().clear();
});

test("a task update recomputes only the tool group that owns that task", async () => {
  const first = makeTask("first-task", "src/first.ts");
  const second = makeTask("second-task", "src/second.ts");
  const turn = makeTurn(first, second);
  useChatViewStore.getState().replaceMessages([assistantMessage(turn)]);
  const rendered = { current: null as ReactTestRenderer | null };
  await act(async () => {
    rendered.current = create(<AgentResultStream turn={turn} />);
  });
  const renderer = rendered.current;
  if (!renderer) {
    throw new Error("Agent result stream did not render");
  }
  expect(harness.presentationCalls).toEqual(["first-task-call", "second-task-call"]);
  harness.presentationCalls.length = 0;

  const updatedSecond = makeTask("second-task", "src/second-updated.ts");
  await act(async () => {
    useChatViewStore.getState().updateAssistantMessages([
      assistantMessage(makeTurn(first, updatedSecond)),
    ]);
  });
  expect(harness.presentationCalls).toEqual(["second-task-call"]);

  await act(async () => renderer.unmount());
});
