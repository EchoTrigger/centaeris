import assert from "node:assert/strict";
import { test } from "node:test";
import { parseAgentOutputPage, mergeAgentOutputs, projectAgentOutputs, parseReferencedSession } from "../../src/agent-chat/agentOutputFeed.ts";

const output = (overrides = {}) => ({ id: "output-1", agentRunId: "run-1", turnId: "turn-1", toolCallId: "call-1", sourceSequence: 3, createdAtMs: Date.parse("2026-09-30T14:30:00Z"), body: "A committed whole reply", sessionRefs: ["work-1"], fileRefs: ["opaque-file-1"], ...overrides });
const page = (overrides = {}) => ({ schema: "agent.messages.v1", agentId: "agent-1", sessionId: "coord-1", messages: [output()], nextAfterSequence: null, ...overrides });

test("the committed output contract binds agent, coordination identity and canonical source order", () => {
  const parsed = parseAgentOutputPage(page({ messages: [output(), output({ id: "output-2", sourceSequence: 7, createdAtMs: Date.parse("2026-09-30T14:29:00Z") })], nextAfterSequence: 10 }), "agent-1", 0);
  assert.deepEqual(parsed.messages.map(message => message.id), ["output-1", "output-2"]);
  assert.equal(parsed.nextAfterSequence, 10);
  assert.throws(() => parseAgentOutputPage(page({ agentId: "other" }), "agent-1", 0));
  assert.throws(() => parseAgentOutputPage(page(), "agent-1", 0, "other-coordination"));
  assert.throws(() => parseAgentOutputPage(page({ messages: [output({ sourceSequence: 3 }), output({ id: "output-2", sourceSequence: 2 })] }), "agent-1", 0));
});

test("an ignored-source page can advance its cursor while empty; cursor regression or unknown schema loud-fails", () => {
  assert.equal(parseAgentOutputPage(page({ messages: [], nextAfterSequence: 9 }), "agent-1", 4).nextAfterSequence, 9);
  for (const nextAfterSequence of [0, 2, 2.5, "9"]) assert.throws(() => parseAgentOutputPage(page({ nextAfterSequence }), "agent-1", 0));
  assert.equal(parseAgentOutputPage(page({ nextAfterSequence: 3 }), "agent-1", 0).nextAfterSequence, 3);
  assert.throws(() => parseAgentOutputPage(page({ schema: "agent.messages.unknown" }), "agent-1", 0));
  assert.throws(() => parseAgentOutputPage(page({ messages: [output({ sourceSequence: 3 })] }), "agent-1", 3));
});

test("output projection never manufactures user history, latest-selection, uptake Read or Library identity", () => {
  const source = parseAgentOutputPage(page(), "agent-1", 0).messages;
  const projected = projectAgentOutputs(source, "Research Agent", new Map([["work-1", { sessionId: "work-1", title: "Actual session title", runState: "unknown" as const }]]));
  assert.equal(projected[0].role, "agent");
  assert.equal(projected[0].text, "A committed whole reply");
  assert.equal(projected[0].createdAt, "2026-09-30T14:30:00.000Z");
  assert.equal(projected[0].isLatest, false);
  assert.equal("loopInputFact" in projected[0], false);
  assert.deepEqual(source[0].fileRefs, ["opaque-file-1"]);
  assert.equal("libraryObjectId" in projected[0], false);
  assert.deepEqual(projected[0].sessions?.map(session => session.title), ["Actual session title"]);
  assert.equal(projectAgentOutputs(source, "Research Agent", new Map())[0].sessions?.length, 0);
});

test("malformed facts and invented receipt fields are rejected at the output adapter", () => {
  for (const fact of [output({ createdAtMs: "now" }), output({ createdAtMs: Infinity }), output({ sourceSequence: -1 }), output({ body: "" }), output({ sessionRefs: [null] }), output({ fileRefs: Array(9).fill("file") }), output({ loopInputFact: { kind: "admission" } }), output({ role: "user" })]) {
    assert.throws(() => parseAgentOutputPage(page({ messages: [fact] }), "agent-1", 0));
  }
});

test("repeated page replay is idempotent, keeps canonical order and rejects changed facts for an existing identity", () => {
  const first = parseAgentOutputPage(page(), "agent-1", 0).messages;
  const second = parseAgentOutputPage(page({ messages: [output({ id: "output-2", sourceSequence: 5, toolCallId: "call-2" })] }), "agent-1", 3).messages;
  const merged = mergeAgentOutputs(first, second);
  assert.deepEqual(mergeAgentOutputs(merged, first).map(message => message.id), ["output-1", "output-2"]);
  assert.deepEqual(first.map(message => message.id), ["output-1"]);
  assert.throws(() => mergeAgentOutputs(first, [output({ body: "altered fact" })]));
});

test("referenced session title must match the authorized workspace and id; activity presence is not terminal run state", () => {
  const detail = { session: { id: "work-1", workspaceId: "workspace-1", agentId: "worker-agent", title: "Actual work", status: "active", hasActiveAgentRun: false } };
  assert.deepEqual(parseReferencedSession(detail, "workspace-1", "work-1"), { sessionId: "work-1", title: "Actual work", runState: "unknown", agentId: "worker-agent" });
  assert.throws(() => parseReferencedSession(detail, "other-workspace", "work-1"));
  assert.throws(() => parseReferencedSession(detail, "workspace-1", "other-session"));
  assert.equal(parseReferencedSession({ session: { ...detail.session, hasActiveAgentRun: true } }, "workspace-1", "work-1").runState, "unknown");
});
