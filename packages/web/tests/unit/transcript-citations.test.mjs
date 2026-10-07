import assert from "node:assert/strict";
import test from "node:test";
import { createTranscriptCitationReader } from "../../src/chat/transcriptCitations.ts";

const tools = [{ sourceSequence: "3", callId: "call" }, { sourceSequence: "7", callId: "call" }];
const citation = (run) => ({ citationId: `citation:${run}`, inputRef: "input", displayName: "Material", sourceToolCallId: "call", sourceUrl: `/api/citations/citation:${run}` });
const response = (sessionId = "session") => ({ sessionId, bindings: tools.map((tool, index) => ({
  sourceSequence: tool.sourceSequence, sourceToolCallId: tool.callId,
  snapshot: { schema: "workspace.citations.v1", sessionId, agentRunId: `run${index}`, throughSequence: 4, citations: [citation(`run${index}`)] },
})) });

test("history and reconnect bind equal call IDs to the exact tool sequence and run", async () => {
  const reader = createTranscriptCitationReader("session", async () => response());
  await reader.refresh(tools);
  assert.deepEqual(reader.getSnapshot().citations.get("3"), [citation("run0")]);
  assert.deepEqual(reader.getSnapshot().citations.get("7"), [citation("run1")]);
  await reader.refresh(tools);
  assert.equal(reader.getSnapshot().citations.get("3").length, 1);
  reader.dispose();
});

test("session switch disposes late requests and a later refresh wins", async () => {
  const pending = [];
  const reader = createTranscriptCitationReader("session", () => new Promise(resolve => pending.push(resolve)));
  const old = reader.refresh(tools);
  const latest = reader.refresh(tools);
  pending[1]({ sessionId: "session", bindings: [] });
  await latest;
  pending[0](response());
  await old;
  assert.equal(reader.getSnapshot().citations.size, 0);
  const late = reader.refresh(tools);
  reader.dispose();
  pending[2](response());
  await late;
  assert.equal(reader.getSnapshot().citations.size, 0);
});

test("wrong session, wrong call, unrequested sequence and duplicate citations fail locally", async () => {
  for (const mutate of [
    value => { value.sessionId = "other"; },
    value => { value.bindings[0].snapshot.sessionId = "other"; },
    value => { value.bindings[0].sourceToolCallId = "other"; },
    value => { value.bindings[0].sourceSequence = "99"; },
    value => { value.bindings[0].snapshot.citations.push(citation("run0")); },
  ]) {
    const value = response(); mutate(value);
    const reader = createTranscriptCitationReader("session", async () => value);
    await reader.refresh(tools);
    assert.equal(reader.getSnapshot().error, true);
    assert.equal(reader.getSnapshot().citations.size, 0);
    reader.dispose();
  }
});

test("stream additions replace snapshots; request failure is local and retry recovers", async () => {
  let value = { sessionId: "session", bindings: [] };
  const reader = createTranscriptCitationReader("session", async () => { if (!value) throw Error("offline"); return value; });
  await reader.refresh(tools);
  value = response(); await reader.refresh(tools);
  assert.equal(reader.getSnapshot().citations.size, 2);
  value = null; await reader.refresh(tools);
  assert.equal(reader.getSnapshot().error, true);
  value = { sessionId: "session", bindings: [] }; await reader.refresh(tools);
  assert.equal(reader.getSnapshot().error, false);
  assert.equal(reader.getSnapshot().citations.size, 0);
  reader.dispose();
});
