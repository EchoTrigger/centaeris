import assert from "node:assert/strict";
import { test } from "vitest";
import {
  applySessionEventToAssistantTurn,
  buildAssistantTurnFromStreamItems,
} from "../src/components/chat/chatTranscriptRestore.ts";
import { normalizeDesktopAgentEventEnvelope } from "../src/lib/chatBridge.ts";
import { getAgentStreamPayloadAgentRunId } from "../src/lib/sessionViewCache.ts";

const envelope = (eventType, eventPayload) => ({
  sessionId: "session-100",
  agentRunId: "agent-run-100",
  payload: {
    type: "runtime_event",
    event: {
      id: `event-${eventType}`,
      type: eventType,
      sessionId: "session-100",
      turnId: "turn-100",
      taskId: "agent-run-100",
      parentTaskId: "turn-100",
      visibility: "user",
      payload: eventPayload,
    },
  },
});

test("binds every desktop stream payload to independent Session and AgentRun identities", () => {
  for (const [eventType, eventPayload] of [
    ["ModelTextDelta", { delta: "partial" }],
    ["ModelTextReplace", { content: "replacement" }],
    ["Final", { content: "complete answer" }],
  ]) {
    const normalized = normalizeDesktopAgentEventEnvelope(
      envelope(eventType, eventPayload),
    );
    assert.equal(normalized.sessionId, "session-100");
    assert.equal(normalized.agentRunId, "agent-run-100");
    assert.equal(normalized.payload.agentRunId, undefined);
    assert.equal(getAgentStreamPayloadAgentRunId(normalized.payload), "");
    assert.equal(normalized.payload.event.taskId, "agent-run-100");
  }
});

test("keeps Final before the terminal session event and projects its complete answer", () => {
  const finalPayload = normalizeDesktopAgentEventEnvelope(
    envelope("Final", { content: "complete answer" }),
  ).payload;
  const terminalPayload = normalizeDesktopAgentEventEnvelope({
    sessionId: "session-100",
    agentRunId: "agent-run-100",
    payload: {
      type: "session_event",
      agentRunId: "agent-run-100",
      event: {
        id: "event-turn-completed",
        type: "AgentRunCompleted",
        at: 200,
        sessionId: "session-100",
        turnId: "turn-100",
        taskId: "agent-run-100",
        parentTaskId: "turn-100",
        visibility: "internal",
        payload: { doneReason: "finalized" },
      },
    },
  }).payload;

  assert.deepEqual(
    [finalPayload, terminalPayload].map((payload) => payload.type),
    ["runtime_event", "session_event"],
  );
  assert.equal(terminalPayload.agentRunId, "agent-run-100");
  assert.equal(getAgentStreamPayloadAgentRunId(terminalPayload), "agent-run-100");
  const turn = buildAssistantTurnFromStreamItems(
    [{ ...finalPayload, type: "session_event" }],
    "",
    0,
  );
  assert.equal(turn.finalAnswer, "complete answer");
});

test("closes an active turn from the committed terminal session event", () => {
  const active = {
    ...buildAssistantTurnFromStreamItems([], "", 0),
    isStreaming: true,
    activity: { kind: "thinking", label: "Thinking" },
  };
  const completed = applySessionEventToAssistantTurn(active, {
    id: "event-turn-completed",
    type: "AgentRunCompleted",
    at: 200,
    taskId: "agent-run-100",
    visibility: "internal",
    payload: { doneReason: "finalized" },
  });

  assert.equal(completed.isStreaming, false);
  assert.equal(completed.activity, undefined);
  assert.equal(completed.completedAtMs, 200);
});

test("restores each committed compaction as one chronological marker", () => {
  const event = {
    id: "event-compaction-1",
    type: "PromptCompaction",
    status: "done",
    at: 150,
    sessionId: "session-100",
    turnId: "turn-100",
    taskId: "agent-run-100",
    parentTaskId: "turn-100",
    visibility: "user",
    payload: {
      compactionId: "compaction-1",
      summaryMessageId: "summary-1",
      summaryMarkdown: "summary",
      firstKeptMessageId: null,
      createdReason: "context_pressure_threshold_reached",
    },
  };
  const restored = buildAssistantTurnFromStreamItems(
    [{ type: "session_event", agentRunId: "agent-run-100", event }],
    "",
    0,
  );
  assert.deepEqual(
    restored.chunks.map(({ text, phase, sourceItemId }) => ({ text, phase, sourceItemId })),
    [{
      text: "Compacted conversation",
      phase: "compaction",
      sourceItemId: "event-compaction-1",
    }],
  );

  const duplicate = applySessionEventToAssistantTurn(restored, event);
  assert.equal(duplicate.chunks.length, 1);
  assert.throws(
    () => applySessionEventToAssistantTurn(restored, { ...event, status: "error" }),
    /只接受已提交的 PromptCompaction/,
  );
});

test("requires both outer identities without rewriting payload identity", () => {
  assert.throws(
    () =>
      normalizeDesktopAgentEventEnvelope({
        ...envelope("Final", { content: "answer" }),
        agentRunId: undefined,
      }),
    /requires agentRunId/,
  );
  const normalized = normalizeDesktopAgentEventEnvelope({
    ...envelope("Final", { content: "answer" }),
    sessionId: "session-200",
  });
  assert.equal(normalized.sessionId, "session-200");
  assert.equal(normalized.agentRunId, "agent-run-100");
});

test("reads replay identity only from committed SessionStreamProjection", () => {
  assert.equal(
    getAgentStreamPayloadAgentRunId({
      type: "session_event",
      agentRunId: "agent-run-100",
      event: {
        type: "Final",
        taskId: "agent-run-100",
        payload: { content: "answer" },
      },
    }),
    "agent-run-100",
  );
});
