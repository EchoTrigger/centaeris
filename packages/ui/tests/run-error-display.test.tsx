import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, test } from "vitest";
import vectors from "../../../tests/fixtures/run-error-display.json";
import { DesktopTranscriptView } from "../src/components/chat/transcriptPaging";
import { applySessionEventToAssistantTurn, buildAssistantTurnFromStreamItems } from "../src/components/chat/chatTranscriptRestore";
import { AgentProcessTranscript } from "../src/components/chat/AgentTranscriptSections";
import { buildTranscriptProcessViewModel } from "../src/components/chat/agentTranscriptModel";
import type { TranscriptPageV1, SessionEvent } from "../src/lib/chatBridge";
import type { AssistantExecutionTurn } from "../src/components/chat/types";

const emptyTurn = (): AssistantExecutionTurn => ({ id: "run", chunks: [], finalAnswer: "", isStreaming: true });
const page = (sourceType: string, reason: string): TranscriptPageV1 => ({
  schema: "transcript.page.v1", sessionId: "session", projectionVersion: "transcript.projection.v1",
  projectionGeneration: "generation", sourceHighWater: "1", hasOlder: false, olderCursor: null,
  resumeCursors: [{ streamId: "session-jsonl.v1", cursor: "1" }],
  blocks: [{ blockId: "run-boundary:event", blockRevision: "1", orderKey: { sourceSequence: "1", ordinal: 0 },
    presentation: { sourceType, agentRunId: "run", observedAtMs: 10, displayTarget: null, durationMs: null, operation: null },
    body: { kind: "notice", noticeType: "run_boundary", content: { inlineContent: reason }, status: "completed" } }],
});
const render = (turn: AssistantExecutionTurn) => renderToStaticMarkup(createElement(AgentProcessTranscript, {
  processTranscript: buildTranscriptProcessViewModel(turn), agentRunId: "run", onOpenWorkspacePath: undefined,
}));

test.each(vectors)("historical failure displays $reason", ({ reason, expected }) => {
  const message = DesktopTranscriptView.open(page("agent_run_failed", reason)).materializeMessages(false)[0];
  if (message.role !== "assistant") throw new Error("missing assistant");
  const html = render(message.turn);
  expect(html).toContain('role="status"');
  expect(html).toContain(expected.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll("'", "&#x27;"));
});

test.each(["AgentRunFailed", "AgentRunInterrupted"])("live %s matches history and replay adds no duplicate", (type) => {
  const event = { id: "event", type, at: 10, payload: { reasonType: "provider_interrupted", message: "provider_timeout" } } as SessionEvent;
  const live = applySessionEventToAssistantTurn(emptyTurn(), event);
  expect(live.isStreaming).toBe(false);
  expect(render(live)).toContain("The model took too long to respond. Retry the request. · provider_timeout");
  expect(render(applySessionEventToAssistantTurn(live, event))).toBe(render(live));
  const message = DesktopTranscriptView.open(page(type === "AgentRunFailed" ? "agent_run_failed" : "agent_run_interrupted", "provider_timeout")).materializeMessages(false)[0];
  if (message.role !== "assistant") throw new Error("missing assistant");
  expect(render(message.turn)).toBe(render(live));
});

test.each(["agent_run_started", "agent_run_completed"])("%s does not render an error", (source) => {
  const message = DesktopTranscriptView.open(page(source, "provider_timeout")).materializeMessages(false)[0];
  if (message.role !== "assistant") throw new Error("missing assistant");
  expect(render(message.turn)).toBe("");
});

test("referenced failures retain their read identity without a byte-count reason", () => {
  const history = page("agent_run_interrupted", "");
  const reference = { refId: "error-ref", revision: "1", byteLength: "70000" };
  history.blocks[0].body = { kind: "notice", noticeType: "run_boundary", status: "completed", content: { sourceRef: reference } };
  const message = DesktopTranscriptView.open(history).materializeMessages(false)[0];
  expect(message.transcriptText).toEqual({ sessionId: "session", projectionGeneration: "generation", reference });
  if (message.role !== "assistant") throw new Error("missing assistant");
  expect(message.turn.chunks).toMatchObject([{ text: "", runFailure: "interrupted" }]);
  expect(render(message.turn)).not.toContain("70000");
});

test("live failure preserves whitespace exactly like its projected history", () => {
  const reason = "  first line\n  more detail  ";
  const event = { id: "event", type: "AgentRunFailed", payload: { reasonType: "model_adapter_failed", message: reason } } as SessionEvent;
  const live = applySessionEventToAssistantTurn(emptyTurn(), event);
  const message = DesktopTranscriptView.open(page("agent_run_failed", reason)).materializeMessages(false)[0];
  if (message.role !== "assistant") throw new Error("missing assistant");
  expect(render(live)).toBe(render(message.turn));
});

test("runtime failure stays provisional until the durable terminal reason replaces it", () => {
  const runtime = { id: "runtime-error", type: "RuntimeError", payload: { message: "provider_timeout", processState: "provider_unavailable" } } as SessionEvent;
  const provisional = applySessionEventToAssistantTurn(emptyTurn(), runtime);
  expect(provisional.isStreaming).toBe(true);
  expect(render(provisional)).toContain("provider_timeout");
  const terminal = { id: "terminal", type: "AgentRunFailed", payload: { reasonType: "model_adapter_failed", message: "provider_timeout" } } as SessionEvent;
  const settled = applySessionEventToAssistantTurn(provisional, terminal);
  expect(settled.isStreaming).toBe(false);
  expect((render(settled).match(/provider_timeout/g) ?? []).length).toBe(1);
  expect(render(applySessionEventToAssistantTurn(settled, runtime))).toBe(render(settled));
});

test("active replay restores the provisional reason even before a terminal event exists", () => {
  const turn = buildAssistantTurnFromStreamItems([{ type: "runtime_event", agentRunId: "run",
    event: { id: "runtime-error", type: "RuntimeError", payload: { message: "provider_timeout" } } }], "");
  expect(render(turn)).toContain("The model took too long to respond. Retry the request. · provider_timeout");
});

test("ordinary error notices preserve their existing rendering", () => {
  const history = page("phase_event", "provider_timeout");
  history.blocks[0].body = { kind: "notice", noticeType: "stage", status: "failed", content: { inlineContent: "provider_timeout" } };
  const message = DesktopTranscriptView.open(history).materializeMessages(false)[0];
  if (message.role !== "assistant") throw new Error("missing assistant");
  const html = render(message.turn);
  expect(html).toContain("provider_timeout");
  expect(html).not.toContain('role="status"');
  expect(html).not.toContain("The model took too long");
});
