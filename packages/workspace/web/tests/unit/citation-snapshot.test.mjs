import assert from "node:assert/strict";
import test from "node:test";
import { validateCitationSnapshot, createCitationRefresher } from "../../src/chat/citationSnapshot.ts";

const summary = { citationId: "citation:1", inputRef: "input", displayName: "Material", sourceToolCallId: "call", sourceUrl: "/api/citations/citation:1" };
const snapshot = (sequence = 2) => ({ schema: "workspace.citations.v1", sessionId: "session", agentRunId: "run", throughSequence: sequence, citations: [summary] });

test("snapshot validates identity, exact fields, duplicates and safe source URLs", () => {
  assert.deepEqual(validateCitationSnapshot(snapshot(), "session", "run"), snapshot());
  for (const bad of [{ ...snapshot(), agentRunId: "other" }, { ...snapshot(), extra: true },
    { ...snapshot(), citations: [summary, summary] },
    { ...snapshot(), citations: [{ ...summary, sourceUrl: "https://external.invalid" }] }]) {
    assert.throws(() => validateCitationSnapshot(bad, "session", "run"));
  }
});

test("refresh merges concurrent requests, follows up after new event, and abort never applies", async () => {
  const pending = [], applied = [];
  const refresh = createCitationRefresher({ sessionId: "session", agentRunId: "run",
    load: () => new Promise((resolve) => pending.push(resolve)), apply: (value) => applied.push(value) });
  const first = refresh.request();
  const second = refresh.request();
  assert.equal(pending.length, 1);
  pending.shift()(snapshot());
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(pending.length, 1);
  refresh.dispose();
  pending.shift()(snapshot(3));
  await Promise.all([first, second]);
  assert.equal(applied.length, 1);
});

test("failed refresh does not clear state and a later refresh can recover", async () => {
  let fail = true;
  let current = snapshot();
  const refresh = createCitationRefresher({ sessionId: "session", agentRunId: "run",
    load: async () => { if (fail) throw new Error("offline"); return snapshot(3); },
    apply: (value) => { current = value; } });
  await assert.rejects(refresh.request(), /offline/);
  assert.equal(current.throughSequence, 2);
  fail = false;
  await refresh.request();
  assert.equal(current.throughSequence, 3);
  refresh.dispose();
});
