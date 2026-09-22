import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { AgentResultStream } from "../src/components/chat/AgentResultStream";
import { useChatViewStore } from "../src/components/chat/chatViewStore";
import { mapProcessStateToActivity } from "../src/components/chat/chatRuntimeCore";
import type { AssistantExecutionTurn, TaskChunk } from "../src/components/chat/types";
const markdown = vi.hoisted(() => vi.fn());
vi.mock("../src/components/chat/MarkdownContent", () => ({ MarkdownContent: ({text}: {text:string}) => { markdown(text); return <p>{text}</p>; } }));
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
let renderer: ReactTestRenderer;
beforeEach(() => { vi.useFakeTimers(); vi.setSystemTime(0); useChatViewStore.getState().clear(); markdown.mockClear(); });
afterEach(() => { act(() => renderer?.unmount()); vi.useRealTimers(); });
const task = (id: string, status: "running" | "done" = "running"): TaskChunk => ({ id, kind: "task", task: { id, title: "read", provider: "tool", summary: "", status, displayTarget: `${id}.md`, operations: [{callId:id,toolName:"read",status}] } });
const base: AssistantExecutionTurn = {id:"run",agentRunId:"run",isStreaming:true,finalAnswer:"",chunks:[]};
function update(turn: AssistantExecutionTurn, mount = false) {
  act(() => {
    useChatViewStore.getState().replaceMessages([{id:"message",role:"assistant",turn}]);
    if (mount) renderer = create(<AgentResultStream turn={turn}/>);
    else renderer.update(<AgentResultStream turn={turn}/>);
  });
}
const elapsed = () => renderer.root.findAllByType("time").map(node => node.children.join(""));
test("newest active task owns the timer; labels do not reset it and older parallel work retains its clock", () => {
  update({...base,chunks:[task("a")]},true);
  expect(elapsed()).toEqual([]);
  act(() => { vi.advanceTimersByTime(18_000); });
  expect(elapsed()).toEqual(["18s"]);
  update({...base,chunks:[{...task("a"),task:{...task("a").task,displayTarget:"renamed.md"}}]});
  expect(elapsed()).toEqual(["18s"]);
  update({...base,chunks:[task("a"),task("b")]});
  expect(elapsed()).toEqual([]);
  act(() => { vi.advanceTimersByTime(2_000); });
  expect(elapsed()).toEqual(["2s"]);
  update({...base,chunks:[task("a"),task("b","done")]});
  expect(elapsed()).toEqual(["20s"]);
  update({...base,isStreaming:false,chunks:[task("a","done"),task("b","done")],finalAnswer:"Done"});
  expect(elapsed()).toEqual([]);
  expect(renderer.root.findAllByProps({className:"runStatusLine"})).toHaveLength(0);
});
test("timer omits zero units, ticks independently of transcript and is never restored from history", () => {
  update({...base,startedAtMs:-999_999,chunks:[task("a"),{id:"stage",kind:"narrative",text:"Stage"}]},true);
  const calls = markdown.mock.calls.length;
  act(() => { vi.advanceTimersByTime(4_818_000); });
  expect(elapsed()).toEqual(["1h 20m 18s"]);
  expect(markdown).toHaveBeenCalledTimes(calls);
  act(() => { vi.advanceTimersByTime(42_000); });
  expect(elapsed()).toEqual(["1h 21m"]);
  act(() => renderer.unmount());
  // A row remount in the same active view preserves its clock.
  act(() => { renderer=create(<AgentResultStream turn={useChatViewStore.getState().turnById.run}/>); });
  expect(elapsed()).toEqual(["1h 21m"]);
  act(() => useChatViewStore.getState().clear());
  update({...base,startedAtMs:0,chunks:[task("a")]});
  expect(elapsed()).toEqual([]);
});
test("completed turns show process and final directly without a Work toggle or retained clock", () => {
  update({...base,isStreaming:false,startedAtMs:0,completedAtMs:999_999,chunks:[{id:"stage",kind:"narrative",text:"Stage"}],finalAnswer:"Done"},true);
  expect(renderer.root.findAllByType("p").map(node=>node.children.join(""))).toEqual(["Stage","Done"]);
  expect(renderer.root.findAllByType("button")).toHaveLength(0);
  expect(elapsed()).toEqual([]);
});
test("waiting remains explicit and never shows a spinning progress indicator", () => {
  const activity = mapProcessStateToActivity("waiting");
  expect(activity?.processState).toBe("waiting");
  update({...base,activity},true);
  expect(renderer.root.findByProps({role:"status"}).children.join("")).toContain("Waiting");
  expect(renderer.root.findAllByProps({className:"runStatusSpinner"})).toHaveLength(0);
});

test("runtime and Tachikoma eggs change presentation without changing task clocks; waiting overrides them", () => {
  update({...base,agentRunId:"runtime-0",activity:{kind:"thinking",label:"Thinking",processState:"thinking"}},true);
  expect(renderer.root.findByProps({role:"status"}).children).toEqual(["a faint signal crossed the Wired…"]);
  act(() => { vi.advanceTimersByTime(18_000); });
  update({...base,agentRunId:"runtime-0",activity:{kind:"thinking",label:"Updated wording",processState:"thinking"}});
  expect(elapsed()).toEqual(["18s"]);
  const chunks: AssistantExecutionTurn["chunks"] = Array.from({length:3},(_,i)=>({id:`agent-${i}`,kind:"subagent",subagent:{id:`agent-${i}`,subagentId:`agent-${i}`,title:"Task",summary:"",status:"running"}}));
  update({...base,agentRunId:"tachikoma-0",chunks});
  expect(renderer.root.findByProps({role:"status"}).children.join("")).toContain("Tachikoma ×3");
  update({...base,agentRunId:"tachikoma-0",chunks,activity:mapProcessStateToActivity("waiting")});
  expect(renderer.root.findByProps({role:"status"}).children).toEqual(["Waiting for your input"]);
  expect(renderer.root.findAllByProps({className:"runStatusSpinner"})).toHaveLength(0);
});
