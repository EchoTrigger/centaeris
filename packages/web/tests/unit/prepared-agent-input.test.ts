import assert from "node:assert/strict";
import { test } from "node:test";
import { prepareAgentInputSubmission, parsePreparedAgentInputAcceptance, parsePreparedAgentInputPage, projectPreparedAgentInputs } from "../../src/agent-chat/preparedAgentInput.ts";

// Synthetic hosted-contract examples. No API/Runtime delivery is exercised here.
const binding = { agentId: "agent-one", sessionId: "coord-one" };
const body = "  Exact input\nUnicode: 世界 😀 e\u0301  ";
const fact = (overrides = {}) => ({ inputId: "browser-input:one", sequence: 1, createdAtMs: Date.parse("2026-09-30T14:30:00Z"), body, attachments: [], read: null, ...overrides });
const accepted = (overrides = {}) => ({ schema: "agent.input.accepted.v1", ...binding, input: fact(), ...overrides });
const page = (overrides = {}) => ({ schema: "agent.inputs.v1", ...binding, inputs: [fact()], nextAfterSequence: null, ...overrides });

test("a prepared submission preserves exact text and the caller's retry identity without inventing transport fields", () => {
  const request = prepareAgentInputSubmission("browser-input:one", body);
  assert.deepEqual(request, { schema: "agent.input.submit.v1", inputId: "browser-input:one", body, attachmentRefs: [] });
  assert.equal(Object.isFrozen(request), true);
  assert.deepEqual(prepareAgentInputSubmission(request.inputId, request.body), request);
  assert.notEqual(prepareAgentInputSubmission(request.inputId, body.normalize("NFC")).body, request.body);
  for (const name of ["sessionId", "modelConfigRef", "thinkingMode", "read"]) assert.equal(name in request, false);
});

test("draft identity and UTF-8 bounds reject invalid text without trimming valid input", () => {
  for (const id of ["", " id", "id ", "世界", "a".repeat(65), "id\n"]) assert.throws(() => prepareAgentInputSubmission(id, body));
  assert.equal(prepareAgentInputSubmission("a".repeat(64), "😀".repeat(16384)).body.length, 32768);
  for (const text of ["", " \n\t", "\u0085", "\u001c", "null\0byte", "\ud800", "\udc00", "😀".repeat(16384) + "a"]) {
    assert.throws(() => prepareAgentInputSubmission("id", text));
  }
  // Python's draft validator considers BOM a non-whitespace character.
  assert.equal(prepareAgentInputSubmission("id", "\ufeff").body, "\ufeff");
});

test("accepted input must match the verified binding and exact submitted identity/body", () => {
  const request = prepareAgentInputSubmission("browser-input:one", body);
  const parsed = parsePreparedAgentInputAcceptance(accepted(), binding, request);
  assert.equal(parsed.input.body, body);
  assert.equal(parsed.input.read, null);
  for (const response of [accepted({ agentId: "other" }), accepted({ sessionId: "other" }), accepted({ schema: "agent.input.accepted.unknown" }), accepted({ input: fact({ inputId: "other" }) }), accepted({ input: fact({ body: body.trim() }) }), accepted({ wakeAck: true })]) {
    assert.throws(() => parsePreparedAgentInputAcceptance(response, binding, request));
  }
});

test("history retains independent input order even when canonical timestamps differ", () => {
  const parsed = parsePreparedAgentInputPage(page({ inputs: [fact({ sequence: 3 }), fact({ inputId: "input-two", sequence: 8, createdAtMs: 1 })] }), binding, 2);
  assert.deepEqual(parsed.inputs.map(input => input.sequence), [3, 8]);
  assert.deepEqual(parsed.inputs.map(input => input.createdAtMs), [Date.parse("2026-09-30T14:30:00Z"), 1]);
  assert.equal("sourceSequence" in parsed.inputs[0], false);
  assert.equal("outputId" in parsed.inputs[0], false);
  assert.throws(() => parsePreparedAgentInputPage(page({ inputs: [fact(), fact({ inputId: "input-two", sequence: 1 })] }), binding, 0));
  assert.throws(() => parsePreparedAgentInputPage(page({ inputs: [fact(), fact({ sequence: 2 })] }), binding, 0));
  assert.throws(() => parsePreparedAgentInputPage(page(), binding, 1));
});

test("a draft history cursor follows its last complete requested page and never infers source progress", () => {
  assert.equal(parsePreparedAgentInputPage(page({ nextAfterSequence: 1 }), binding, 0, 1).nextAfterSequence, 1);
  assert.equal(parsePreparedAgentInputPage(page({ inputs: [], nextAfterSequence: null }), binding, 9).inputs.length, 0);
  for (const limit of [0, 101, 1.5]) assert.throws(() => parsePreparedAgentInputPage(page(), binding, 0, limit));
  for (const after of [-1, 0.5, NaN]) assert.throws(() => parsePreparedAgentInputPage(page(), binding, after));
  for (const response of [page({ nextAfterSequence: 1 }), page({ nextAfterSequence: 7 }), page({ inputs: [], nextAfterSequence: 7 })]) {
    assert.throws(() => parsePreparedAgentInputPage(response, binding, 0));
  }
});

test("unknown identities, extra fields and malformed canonical facts loud-fail", () => {
  for (const response of [page({ agentId: "other" }), page({ sessionId: "other" }), page({ schema: "agent.inputs.unknown" }), page({ handled: true }), page({ inputs: [fact({ createdAtMs: "now" })] }), page({ inputs: [fact({ createdAtMs: -1 })] }), page({ inputs: [fact({ createdAtMs: 9e15 })] }), page({ inputs: [fact({ body: "" })] }), page({ inputs: [fact({ sourceSequence: 1 })] })]) {
    assert.throws(() => parsePreparedAgentInputPage(response, binding, 0));
  }
});

test("legacy uptake shapes and admission or wake acknowledgements fail the exact Read contract", () => {
  for (const read of [{ agentRunId: "run-one", factRef: "committed-start", atMs: 1 }, { kind: "admission", atMs: 1 }, { kind: "queueAck", atMs: 1 }, { handled: true }, true]) {
    assert.throws(() => parsePreparedAgentInputPage(page({ inputs: [fact({ read })] }), binding, 0));
    assert.throws(() => parsePreparedAgentInputAcceptance(accepted({ input: fact({ read }) }), binding, prepareAgentInputSubmission("browser-input:one", body)));
  }
});

test("Read requires exactly the canonical run, event, request and server timestamp", () => {
  const read = { agentRunId: "run", eventId: "event", requestId: "request", createdAtMs: 4 };
  assert.deepEqual(parsePreparedAgentInputPage(page({ inputs: [fact({ read })] }), binding, 0).inputs[0].read, read);
  for (const malformed of [{ ...read, eventId: "" }, { ...read, requestId: " padded " }, { ...read, createdAtMs: "now" }, { ...read, createdAtMs: -1 }, { ...read, handled: true }, { ...read, agentRunId: 1 }]) {
    assert.throws(() => parsePreparedAgentInputPage(page({ inputs: [fact({ read: malformed })] }), binding, 0));
  }
});

test("presentation projection supplies canonical user text/time without manufacturing latest-selection or Read", () => {
  const response = page();
  const parsed = parsePreparedAgentInputPage(response, binding, 0);
  response.inputs[0].body = "changed transport object";
  const views = projectPreparedAgentInputs(parsed.inputs, "You");
  assert.equal(views[0].role, "user");
  assert.equal(views[0].messageId, "browser-input:one");
  assert.equal(views[0].text, body);
  assert.equal(views[0].createdAt, "2026-09-30T14:30:00.000Z");
  assert.equal(views[0].isLatest, false);
  assert.equal("loopInputFact" in views[0], false);
});
