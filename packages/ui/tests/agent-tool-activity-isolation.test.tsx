import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { beforeEach, expect, test, vi } from "vitest";
import type {
  AssistantExecutionTurn,
  ChatMessage,
  TaskResult,
} from "../src/components/chat/types";

type AssistantChatMessage = Extract<ChatMessage, { role: "assistant" }>;

const harness = vi.hoisted(() => ({
  detailCalls: [] as string[],
}));

vi.mock("../src/components/chat/toolActivityTranscriptModel", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../src/components/chat/toolActivityTranscriptModel")>();
  return { ...actual, getOperationDetailState(...args: Parameters<typeof actual.getOperationDetailState>) {
    harness.detailCalls.push(args[0].callId);
    return actual.getOperationDetailState(...args);
  }};
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
  harness.detailCalls.length = 0;
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
  expect(harness.detailCalls).toEqual(["first-task-call", "second-task-call"]);
  harness.detailCalls.length = 0;

  const updatedSecond = makeTask("second-task", "src/second-updated.ts");
  await act(async () => {
    useChatViewStore.getState().updateAssistantMessages([
      assistantMessage(makeTurn(first, updatedSecond)),
    ]);
  });
  expect(harness.detailCalls).toEqual(["second-task-call"]);

  await act(async () => renderer.unmount());
});


test("Tool detail disclosure survives virtual unmount and appending a tool", async () => {
  const first = {...makeTask("persistent", "one.rs"),modelContent:"file contents"};
  const turn: AssistantExecutionTurn = { id: "persistent-turn", chunks: [{ id: first.id, kind: "task", task: first }], finalAnswer: "", isStreaming: false };
  let renderer: ReactTestRenderer;
  await act(async () => { renderer = create(<AgentResultStream turn={turn} />); });
  const toggle = () => renderer!.root.findAllByProps({ className: "agent-operation-summary agent-tool-node-summary" })[0];
  await act(async () => { toggle().props.onClick({ preventDefault() {} }); });
  expect(toggle().props["aria-expanded"]).toBe(true);
  await act(async () => { renderer!.unmount(); });
  const second = makeTask("appended", "two.rs");
  await act(async () => { renderer = create(<AgentResultStream turn={{ ...turn, chunks: [...turn.chunks, { id: second.id, kind: "task", task: second }] }} />); });
  expect(toggle().props["aria-expanded"]).toBe(true);
  await act(async () => { renderer!.unmount(); });
});

 test("tool titles are ready immediately and unrelated disclosure stays isolated", async () => {
  const tasks = [makeTask("a", "a.rs"), makeTask("b", "b.rs")];
  const turn: AssistantExecutionTurn = {id:"lazy",chunks:tasks.map(task=>({id:task.id,kind:"task",task})),finalAnswer:"",isStreaming:false};
  let renderer: ReactTestRenderer;
  await act(async()=> {renderer=create(<AgentResultStream turn={turn}/>);});
  expect(harness.detailCalls).toEqual(["a-call","b-call"]);
  harness.detailCalls.length=0;
  await act(async()=> useChatViewStore.setState({expandedTools:{...useChatViewStore.getState().expandedTools,"unrelated":true}}));
  expect(harness.detailCalls).toEqual([]);
  await act(async()=>renderer!.unmount());
});
