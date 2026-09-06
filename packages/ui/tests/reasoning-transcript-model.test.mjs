import { expect, test } from "vitest";
import { readFileSync } from "node:fs";
import { buildTranscriptProcessViewModel } from "../src/components/chat/agentTranscriptModel";
import { applySessionEventToAssistantTurn } from "../src/components/chat/chatTranscriptRestore";
import { reasoningPreview } from "../src/components/chat/reasoningPreview";

test("live reasoning and answer replace atomically, reject older revisions and cannot reopen a seal", () => {
  const reasoning = JSON.parse(readFileSync(new URL("../../core/tests/fixtures/live_reasoning.json", import.meta.url), "utf8"));
  expect(reasoningPreview(reasoning.text)).toBe("核对 input 保留 code 与 来源");
  const snapshot = (revision, text, value = reasoning) => ({ id: `live:${revision}`, type: "ModelSnapshot", turnId: "turn-1", payload: { revision, text, reasoning: value } });
  let turn = applySessionEventToAssistantTurn({ chunks: [], finalAnswer: "", isStreaming: true }, snapshot(2, "answer"));
  expect(turn.chunks[0]).toMatchObject({ id: reasoning.blockId, text: reasoning.text, status: "streaming" });
  expect(turn.finalAnswer).toBe("answer");
  expect(applySessionEventToAssistantTurn(turn, snapshot(1, "old answer"))).toBe(turn);
  turn = applySessionEventToAssistantTurn(turn, { id: "seal", type: "Reasoning", turnId: "turn-1", payload: { ...reasoning, status: "interrupted" } });
  const late = applySessionEventToAssistantTurn(turn, snapshot(3, "answer", { ...reasoning, text: "late" }));
  expect(late.chunks).toHaveLength(1);
  expect(late.chunks[0]).toMatchObject({ text: reasoning.text, status: "interrupted" });
  const ended = { ...late, isStreaming: false };
  expect(applySessionEventToAssistantTurn(ended, snapshot(4, "late answer"))).toBe(ended);
});

test("committed reasoning restores verbatim before the pending answer and rejects conflicting seals", () => {
  const turn = { chunks: [], finalAnswer: "Answer", isStreaming: true };
  const payload = JSON.parse(readFileSync(new URL("../../core/tests/fixtures/reasoning_block.json", import.meta.url), "utf8"));
  const event = { id: "evt-r1", type: "Reasoning", status: "done", visibility: "user", turnId: "turn-1", payload };
  const restored = applySessionEventToAssistantTurn(turn, event);
  expect(restored.chunks).toEqual([{ id: payload.blockId, kind: "reasoning", turnId: "turn-1", text: " inspect\n", status: "done" }]);
  expect(restored.finalAnswer).toBe("Answer");
  expect(applySessionEventToAssistantTurn(restored, event).chunks).toHaveLength(1);
  expect(() => applySessionEventToAssistantTurn(restored, { ...event, payload: { ...event.payload, text: "changed" } })).toThrow();
});

const tool = (id) => ({ id, kind: "task", task: {
  id, title: "read", summary: "", provider: "tool", status: "done",
} });

test("reasoning is a peer that separates consecutive tool groups and keeps source order", () => {
  const chunks = [tool("a"), tool("b"),
    { id: "r1", kind: "reasoning", text: "Inspect", status: "streaming" },
    tool("c"), { id: "r2", kind: "reasoning", text: "Check", status: "done" },
    { id: "note", kind: "narrative", text: "Done" }];
  const before = structuredClone(chunks);
  const view = buildTranscriptProcessViewModel({ chunks });
  expect(view.processItems.map((item) => item.kind)).toEqual([
    "toolGroup", "reasoning", "toolGroup", "reasoning", "assistantText",
  ]);
  expect(view.processItems[0].tasks.map((task) => task.id)).toEqual(["a", "b"]);
  expect(view.processItems[2].tasks.map((task) => task.id)).toEqual(["c"]);
  expect(view.processSections[0].items.map((item) => item.id)).toEqual(["a-activity", "r1", "c-activity", "r2"]);
  expect(chunks).toEqual(before);
  const completed = buildTranscriptProcessViewModel({ chunks: chunks.map((chunk) =>
    chunk.id === "r1" ? { ...chunk, status: "done", text: "Inspect more" } : chunk) });
  expect(completed.processItems.map((item) => item.id)).toEqual(view.processItems.map((item) => item.id));
});
