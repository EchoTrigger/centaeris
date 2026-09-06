import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, expect, test, vi } from "vitest";
import { AgentResultStream } from "../src/components/chat/AgentResultStream";
import { useChatViewStore } from "../src/components/chat/chatViewStore";
import type { AssistantExecutionTurn } from "../src/components/chat/types";

vi.mock("../src/components/chat/MarkdownContent", () => ({
  MarkdownContent: ({ text }: { text: string }) => <p>{text}</p>,
}));
let renderer: ReactTestRenderer;
afterEach(() => { act(() => renderer?.unmount()); useChatViewStore.getState().clear(); });

test("reasoning disclosure survives updates, completion and row remount without opening another block", () => {
  const turn: AssistantExecutionTurn = {
    id: "turn-a", agentRunId: "run-a", finalAnswer: "", isStreaming: true,
    activity: { kind: "thinking", label: "Thinking", processState: "thinking" },
    chunks: [{ id: "r1", kind: "reasoning", text: "Inspect", status: "streaming" }],
  };
  act(() => { renderer = create(<AgentResultStream turn={turn} />); });
  expect(renderer.root.findAll((node) => node.props.className === "agentStatusRow")).toHaveLength(0);
  const button = () => renderer.root.findAllByType("button")[0];
  expect(button().props["aria-label"]).toBe("正在思考");
  expect(renderer.root.findByProps({ className: "reasoningPreviewText" }).children).toEqual(["Inspect"]);
  expect(button().props["aria-expanded"]).toBe(false);
  expect(renderer.root.findAllByType("p")).toHaveLength(0);
  act(() => button().props.onClick());
  expect(button().props["aria-expanded"]).toBe(true);
  expect(button().props["aria-label"]).toBe("正在思考");
  expect(renderer.root.findAllByProps({ className: "reasoningPreviewText" })).toHaveLength(0);
  expect(renderer.root.find((node) => node.props.className === "agentReasoningBody").props.tabIndex).toBe(0);
  const updated: AssistantExecutionTurn = { ...turn, isStreaming: false, chunks: [
    { id: "r1", kind: "reasoning", text: "Inspect more", status: "done" },
    { id: "r2", kind: "reasoning", text: "Separate", status: "interrupted" },
  ] };
  act(() => renderer.update(<AgentResultStream turn={updated} />));
  expect(button().props["aria-label"]).toBe("思考");
  expect(renderer.root.findAllByType("button").map((item) => item.props["aria-expanded"])).toEqual([true, false]);
  expect(renderer.root.findByType("p").children).toEqual(["Inspect more"]);
  act(() => renderer.unmount());
  act(() => { renderer = create(<AgentResultStream turn={updated} />); });
  expect(button().props["aria-expanded"]).toBe(true);
  act(() => renderer.update(<AgentResultStream turn={{ ...updated, agentRunId: "run-b" }} />));
  expect(button().props["aria-expanded"]).toBe(false);
  act(() => renderer.update(<AgentResultStream turn={updated} />));
  expect(button().props["aria-expanded"]).toBe(true);
  act(() => button().props.onClick());
  expect(renderer.root.findAllByType("p")).toHaveLength(0);
});
