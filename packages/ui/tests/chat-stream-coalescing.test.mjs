import assert from "node:assert/strict";
import { test } from "vitest";
import { appendCoalescedStreamPayload } from "../src/lib/streamPayloadCoalescing.ts";

const runtimeEvent = (type, payload, id) => ({
  type: "runtime_event",
  event: {
    id,
    type,
    sessionId: "chat-1",
    turnId: "turn-1",
    agentRunId: "turn-1",
    payload,
  },
});

test("coalesces one hundred thousand adjacent deltas without changing the final body", () => {
  const buffer = { items: [], cursor: 0 };
  let expected = "";
  for (let index = 0; index < 100_000; index += 1) {
    const delta = index % 3 === 0 ? "x" : index % 3 === 1 ? " " : "\n";
    expected += delta;
    appendCoalescedStreamPayload(
      buffer,
      runtimeEvent("ModelTextDelta", { delta }, `delta-${index}`),
    );
  }
  appendCoalescedStreamPayload(
    buffer,
    runtimeEvent("Final", { content: expected }, "final-1"),
  );

  assert.equal(buffer.items.length, 2);
  assert.equal(buffer.items[0].event.payload.delta, expected);
  assert.equal(buffer.items[1].event.payload.content, expected);
});

test("never coalesces text across an ordered semantic event", () => {
  const buffer = { items: [], cursor: 0 };
  appendCoalescedStreamPayload(
    buffer,
    runtimeEvent("ModelTextDelta", { delta: "before" }, "delta-before"),
  );
  appendCoalescedStreamPayload(
    buffer,
    runtimeEvent(
      "ToolCallReady",
      { callId: "call-1", argsJson: "{}" },
      "tool-ready",
    ),
  );
  appendCoalescedStreamPayload(
    buffer,
    runtimeEvent("ModelTextDelta", { delta: "after" }, "delta-after"),
  );

  assert.deepEqual(
    buffer.items.map((item) => item.event.type),
    ["ModelTextDelta", "ToolCallReady", "ModelTextDelta"],
  );
});
