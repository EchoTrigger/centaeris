import { expect, test } from "vitest";
import { buildTranscriptFinalItem, buildTranscriptProcessViewModel } from "../src/renderer/components/chat/agentTranscriptModel";
import type { AssistantExecutionTurn } from "../src/renderer/components/chat/types";

test("final transcript identity stays stable across stream completion", () => {
  const streaming = buildTranscriptFinalItem({
    id: "turn-one",
    finalAnswer: "Answer",
    isStreaming: true,
  });
  const completed = buildTranscriptFinalItem({
    id: "turn-one",
    finalAnswer: "Answer",
    isStreaming: false,
  });

  expect(streaming).toMatchObject({
    id: "turn-one-answer-text",
    phase: "streaming",
    text: "Answer",
    waterfall: {
      section: "final",
      groupId: "turn:turn-one:final",
      displayRole: "assistant_final_streaming",
    },
  });
  expect(completed).toMatchObject({
    id: "turn-one-answer-text",
    phase: "final",
    text: "Answer",
    waterfall: {
      section: "final",
      groupId: "turn:turn-one:final",
      displayRole: "assistant_final",
    },
  });
});

test("blank final answers do not create transcript items", () => {
  expect(buildTranscriptFinalItem({
    id: "turn-one",
    finalAnswer: "  ",
    isStreaming: false,
  })).toBeNull();
});

test("mixed tools share a card across Agent lifecycle updates and separate at visible reasoning", () => {
  const tool = (id: string, toolName: string): AssistantExecutionTurn["chunks"][number] => ({ id, kind: "task", task: {
    id, title: toolName, summary: "", status: "done", provider: "tool", operations: [{ callId: id, toolName, status: "done" }],
  } });
  const model = buildTranscriptProcessViewModel({ chunks: [
    tool("write", "write"),
    { id: "agent", kind: "subagent", subagent: { id: "child", subagentId: "child", title: "Work", summary: "", status: "running" } },
    tool("command", "bash"),
    { id: "thinking", kind: "reasoning", text: "Next", status: "done" },
    tool("read", "read"),
  ] });
  expect(model.processItems.map(entry => entry.kind)).toEqual(["toolGroup", "reasoning", "toolGroup"]);
  expect(model.processItems[0].kind === "toolGroup" && model.processItems[0].tasks.map(task => task.id)).toEqual(["write", "command"]);
});
