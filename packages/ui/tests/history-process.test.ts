import { expect, test } from "vitest";
import { historyProcessEntries, historyProcessTiming } from "../src/components/chat/historyProcessEntries";
import type { ChatMessage } from "../src/components/chat/types";

const tool = (id: string, name = "read"): ChatMessage => ({ id, role: "assistant", turn: {
  id, isStreaming: false, finalAnswer: "", chunks: [{ kind: "task", id,
    task: { id, title: name, summary: "", provider: "tool", status: "done", operations: [{ callId: id, toolName: name, status: "done" }] } }],
} });
const text = (id: string): ChatMessage => ({ id, role: "assistant", turn: { id, chunks: [], finalAnswer: id, isStreaming: false } });

test("history groups adjacent tools while keeping intermediate text in order", () => {
  const entries = historyProcessEntries([tool("a"), tool("b"), text("progress"), tool("c"), text("final")]);
  expect(entries.map(entry => entry.id)).toEqual(["a", "progress", "c", "final"]);
  expect(entries[0].role === "assistant" && entries[0].turn.chunks.length).toBe(2);
  expect(historyProcessEntries([tool("a"), tool("b"), tool("c")])[0].id).toBe("a");
});

test("history grouping respects visible reasoning and tool families", () => {
  const reasoning: ChatMessage = { id: "reason", role: "assistant", turn: { id: "reason", finalAnswer: "", isStreaming: false,
    chunks: [{ id: "reason", kind: "reasoning", text: "why", status: "done" }] } };
  expect(historyProcessEntries([tool("a"), reasoning, tool("b"), tool("run", "bash")]).map(entry => entry.id))
    .toEqual(["a", "reason", "b", "run"]);
});


test("history timing pairs actual run boundaries and ignores tool latency", () => {
  const boundary = (id: string, run: string, start?: number, end?: number): ChatMessage => ({ id, role: "assistant", turn: {
    id, chunks: [], finalAnswer: "", isStreaming: false, projectionRunId: run, startedAtMs: start, completedAtMs: end,
  } });
  const messages = [boundary("start", "run", 1000), tool("a"), tool("b"), text("final"), boundary("end", "run", undefined, 4000)];
  expect(historyProcessTiming(messages)).toEqual({ startedAtMs: 1000, completedAtMs: 4000 });
  expect(historyProcessEntries(messages).map(m => m.id)).toEqual(["a", "final"]);
  expect(historyProcessTiming([boundary("start", "a", 1000), boundary("end", "b", undefined, 4000)])).toEqual({});
});
