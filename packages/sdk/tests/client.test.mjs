import assert from "node:assert/strict";
import { setImmediate } from "node:timers/promises";
import { createInterface } from "node:readline";
import { PassThrough } from "node:stream";
import test from "node:test";
import {
  AgentSession,
  RpcConnectionClosedError,
  RpcError,
  RpcProtocolError,
  SessionClient,
} from "../src/index.ts";
import { createJsonlTransport } from "../src/node.ts";

const descriptor = {
  status: "ok",
  runtime: "centaeris-runtime",
  protocol: "centaeris.runtime",
  protocolVersion: 1,
  capabilities: ["json_rpc_2_over_jsonl", "session_log"],
  events: ["session/update", "runtime/config-changed"],
  projections: ["runtime_event", "session_event"],
  buildId: "fixture-build",
  coreProtocolVersion: "fixture-core",
  profileId: "fixture-profile",
  storeId: "fixture-store",
  storeSchemaVersion: 1,
  layoutSchemaVersion: 1,
};

function wireFixture() {
  const requests = new PassThrough();
  const responses = new PassThrough();
  const lines = createInterface({ input: requests, crlfDelay: Infinity });
  const iterator = lines[Symbol.asyncIterator]();
  return {
    requests,
    responses,
    transport: createJsonlTransport(responses, requests),
    async next() {
      const line = await iterator.next();
      assert.equal(line.done, false, "connection closed before receiving a request");
      return JSON.parse(line.value);
    },
    respond(request, result) {
      responses.write(`${JSON.stringify({ jsonrpc: "2.0", id: request.id, result })}\n`);
    },
    notify(params) {
      responses.write(`${JSON.stringify({ jsonrpc: "2.0", method: "session/update", params })}\n`);
    },
  };
}

async function connected(t, registration = { viewerId: "sdk-viewer" }) {
  const fixture = wireFixture();
  const pending = SessionClient.connect(fixture.transport, registration);
  const initialize = await fixture.next();
  assert.equal(initialize.method, "initialize");
  assert.deepEqual(initialize.params.request, {
    viewerId: registration.viewerId,
    clientKind: registration.clientKind ?? "sdk",
  });
  fixture.respond(initialize, descriptor);
  const client = await pending;
  t.after(() => client.close());
  return { ...fixture, client };
}

function sessionItem(id) {
  return {
    id,
    title: "Fixture Session",
    updatedAt: 1,
    lastMessage: null,
    cwd: "/fixture",
    sessionKind: "main",
    parentSessionId: null,
    runtimeJobId: null,
    sortOrder: 0,
    isPinned: false,
    isUnread: false,
    messageCount: 0,
    activityState: "idle",
  };
}

function runSummary(status) {
  return { ...runtimeRun(status), unread: false };
}

function runtimeRun(status) {
  return {
    agentRunId: "run-1",
    sessionId: "session-1",
    turnId: "turn-1",
    agentRunKind: "main",
    cwd: "/fixture",
    status,
    startedAtMs: 1,
    updatedAtMs: 2,
    completedAtMs: null,
    lastEventAtMs: 2,
    stallReason: null,
    watchdog: null,
    error: null,
  };
}

function sessionData(id, messages = []) {
  return {
    id,
    title: "Existing",
    createdAt: 1,
    updatedAt: 2,
    sessionKind: "main",
    parentSessionId: null,
    runtimeJobId: null,
    messages,
  };
}

function persistedMessage() {
  return {
    id: "message-1",
    sessionId: "session-1",
    turnId: "turn-1",
    role: "assistant",
    content: "Fixture response",
    createdAtMs: 1,
    updatedAtMs: 2,
  };
}

test("real duplex JSONL correlates out-of-order create and load responses", { timeout: 5000 }, async (t) => {
  const fixture = await connected(t, { viewerId: "desktop-viewer", clientKind: "desktop" });
  assert.deepEqual(fixture.client.descriptor, descriptor);
  const create = fixture.client.create({ operationId: "create-once", cwd: "/fixture", title: "Title" });
  const load = fixture.client.load({ sessionId: "existing-session" });
  const first = await fixture.next();
  const second = await fixture.next();
  assert.notEqual(first.id, second.id);
  assert.equal(first.method, "session/new");
  assert.deepEqual(first.params, { request: { operationId: "create-once", cwd: "/fixture", title: "Title" } });
  assert.equal(second.method, "session/load");
  assert.deepEqual(second.params, { request: { sessionId: "existing-session" } });
  const loaded = sessionData("existing-session");
  fixture.respond(second, loaded);
  fixture.respond(first, sessionItem("created-session"));
  assert.deepEqual(await load, loaded);
  assert.deepEqual(await create, sessionItem("created-session"));
});

test("send returns admission while updates retain canonical payload and explicit identities", { timeout: 5000 }, async (t) => {
  const fixture = await connected(t);
  const updates = [];
  const stop = fixture.client.onUpdate((update) => updates.push(update));
  let admitted = false;
  const sending = fixture.client.send({
    operationId: "prompt-once", sessionId: "session-1", message: "你好😀",
    attachments: [{ placeholder: "image", localPath: "/fixture/image.png" }],
  }).then((receipt) => { admitted = true; return receipt; });
  const request = await fixture.next();
  assert.equal(request.method, "session/prompt");
  assert.equal(request.params.request.operationId, "prompt-once");
  assert.deepEqual(request.params.request.attachments, [{ placeholder: "image", localPath: "/fixture/image.png" }]);
  const live = {
    type: "runtime_event",
    event: { sessionId: "session-1", turnId: "turn-1", taskId: "turn-1", type: "ModelSnapshot", payload: { text: "你好😀" } },
  };
  const encoded = Buffer.from(`${JSON.stringify({ jsonrpc: "2.0", method: "session/update", params: live })}\r\n`);
  const emoji = encoded.indexOf(Buffer.from("😀"));
  fixture.responses.write(encoded.subarray(0, emoji + 2));
  fixture.responses.write(encoded.subarray(emoji + 2));
  await setImmediate();
  assert.equal(admitted, false);
  assert.equal(updates.length, 1);
  assert.equal(updates[0].sessionId, "session-1");
  assert.equal(updates[0].turnId, "turn-1");
  assert.equal(updates[0].agentRunId, undefined);
  assert.deepEqual(updates[0].params, live);
  const receipt = { sessionId: "session-1", agentRunId: "run-1", turnId: "turn-1", streamItems: [] };
  fixture.respond(request, receipt);
  assert.deepEqual(await sending, receipt);
  const terminal = {
    type: "session_event", agentRunId: "run-1", cursor: "2",
    event: { sessionId: "session-1", turnId: "turn-1", type: "TurnCompleted", status: "completed", payload: {} },
  };
  fixture.notify(terminal);
  await setImmediate();
  assert.equal(updates[1].agentRunId, "run-1");
  assert.deepEqual(updates[1].params, terminal);
  stop();
  fixture.notify(terminal);
  await setImmediate();
  assert.equal(updates.length, 2);
});

test("subscribe binds the initialized viewer and cancel preserves acceptance separately from terminal state", { timeout: 5000 }, async (t) => {
  const fixture = await connected(t);
  const attaching = fixture.client.subscribe({ agentRunId: "run-1", sessionId: "session-1" });
  const attach = await fixture.next();
  assert.equal(attach.method, "_centaeris/session/agent-runs/attach");
  assert.deepEqual(attach.params.request, { agentRunId: "run-1", sessionId: "session-1", viewerId: "sdk-viewer" });
  const attached = { agentRun: runSummary("running"), viewerId: "sdk-viewer", transitionReason: "viewer_attached", attachedViewerCount: 1 };
  fixture.respond(attach, attached);
  assert.deepEqual(await attaching, attached);
  const cancelling = fixture.client.cancel({ agentRunId: "run-1", reason: "user_cancelled" });
  const cancel = await fixture.next();
  assert.equal(cancel.method, "_centaeris/session/agent-runs/cancel");
  assert.deepEqual(cancel.params.request, { agentRunId: "run-1", reason: "user_cancelled" });
  fixture.respond(cancel, { agentRun: runSummary("running"), cancelAccepted: true });
  const receipt = await cancelling;
  assert.equal(receipt.cancelAccepted, true);
  assert.equal(receipt.agentRun.status, "running");
});

test("Session controls use the existing methods and preserve their receipts", { timeout: 5000 }, async (t) => {
  const fixture = await connected(t);
  const cases = [
    {
      method: "supplement", rpc: "_centaeris/session/supplement",
      request: { sessionId: "session-1", agentRunId: "run-1", message: "continue with tests" },
      result: { accepted: true, sessionId: "session-1", agentRunId: "run-1", queuedCount: 1 },
    },
    {
      method: "answerNow", rpc: "_centaeris/session/answer_now",
      request: { schema: "agent_run.intervention.v1", interventionId: "answer-1", agentRunId: "run-1", kind: "answer_now" },
      result: { accepted: false, disposition: "alreadyConverging", sessionId: "session-1", agentRunId: "run-1", interventionId: "answer-1" },
    },
    {
      method: "answerQuestion", rpc: "_centaeris/session/answer_question",
      request: { sessionId: "session-1", questionId: "question-1", answers: ["first"], answerText: null, autoContinueAfterResumeWait: false },
      result: { sessionId: "session-1", agentRunId: "run-1", turnId: "turn-1", streamItems: [] },
    },
    {
      method: "compactContext", rpc: "_centaeris/session/compact_context",
      request: { sessionId: "session-1" },
      result: { sessionId: "session-1", compacted: true },
    },
    {
      method: "listRuns", rpc: "_centaeris/session/agent-runs",
      request: { sessionId: "session-1", includeTerminal: true },
      result: { agentRuns: [runSummary("running")] },
    },
  ];
  for (const specification of cases) {
    const result = fixture.client[specification.method](specification.request);
    const request = await fixture.next();
    assert.equal(request.method, specification.rpc);
    assert.deepEqual(request.params.request, specification.request);
    fixture.respond(request, specification.result);
    assert.deepEqual(await result, specification.result);
  }
});

test("AgentSession binds its identities and rejects request fields that could override them", { timeout: 5000 }, async (t) => {
  const fixture = await connected(t);
  const creating = fixture.client.createSession({ operationId: "bound-create", cwd: "/fixture" });
  const create = await fixture.next();
  assert.equal(create.method, "session/new");
  fixture.respond(create, sessionItem("session-1"));
  const session = await creating;
  assert.ok(session instanceof AgentSession);
  assert.equal(session.id, "session-1");
  assert.throws(() => { session.id = "session-2"; }, TypeError);
  let invalidDispatches = 0;
  const trackInvalidDispatch = () => { invalidDispatches++; };
  fixture.requests.on("data", trackInvalidDispatch);
  const invalidIdentities = [null, "", " \t", 7];
  for (const identity of [undefined, ...invalidIdentities]) {
    assert.throws(() => new AgentSession(fixture.client, identity), TypeError);
    await assert.rejects(session.cancel(identity), TypeError);
    assert.throws(() => session.supplement(identity, "include tests"), TypeError);
  }
  for (const identity of invalidIdentities) {
    await assert.rejects(session.subscribe(identity), TypeError);
  }
  fixture.requests.off("data", trackInvalidDispatch);
  assert.equal(invalidDispatches, 0);
  for (const extra of [{ sessionId: "session-2" }, { session_id: "session-2" }, { unexpected: true }]) {
    assert.throws(() => session.send({ operationId: "bound-send", message: "hello", ...extra }), TypeError);
    assert.throws(() => session.answerQuestion({ questionId: "question-1", ...extra }), TypeError);
  }
  const cases = [
    {
      invoke: () => session.load(), rpc: "session/load", request: { sessionId: "session-1" },
      result: sessionData("session-1"),
    },
    {
      invoke: () => session.send({ operationId: "bound-send", message: "hello" }), rpc: "session/prompt",
      request: { operationId: "bound-send", message: "hello", sessionId: "session-1" },
      result: { sessionId: "session-1", agentRunId: "run-1", turnId: "turn-1", streamItems: [] },
    },
    {
      invoke: () => session.subscribe("run-1"), rpc: "_centaeris/session/agent-runs/attach",
      request: { sessionId: "session-1", agentRunId: "run-1", viewerId: "sdk-viewer" },
      result: { agentRun: runSummary("running"), viewerId: "sdk-viewer", transitionReason: "viewer_attached", attachedViewerCount: 1 },
      expected: { agentRun: runtimeRun("running"), viewerId: "sdk-viewer", transitionReason: "viewer_attached", attachedViewerCount: 1 },
    },
    {
      invoke: () => session.supplement("run-1", "include tests"), rpc: "_centaeris/session/supplement",
      request: { sessionId: "session-1", agentRunId: "run-1", message: "include tests" },
      result: { accepted: true, sessionId: "session-1", agentRunId: "run-1", queuedCount: 1 },
    },
    {
      invoke: () => session.answerQuestion({ questionId: "question-1", answerText: "first" }), rpc: "_centaeris/session/answer_question",
      request: { questionId: "question-1", answerText: "first", sessionId: "session-1" },
      result: { sessionId: "session-1", agentRunId: "run-1", turnId: "turn-1", streamItems: [] },
    },
    {
      invoke: () => session.cancel("run-1", "user_cancelled"), rpc: "_centaeris/session/agent-runs/cancel",
      request: { sessionId: "session-1", agentRunId: "run-1", reason: "user_cancelled" },
      result: { agentRun: runSummary("running"), cancelAccepted: true },
      expected: { agentRun: runtimeRun("running"), cancelAccepted: true },
    },
    {
      invoke: () => session.compactContext(), rpc: "_centaeris/session/compact_context",
      request: { sessionId: "session-1" }, result: { sessionId: "session-1", compacted: true },
    },
    {
      invoke: () => session.listRuns(true), rpc: "_centaeris/session/agent-runs",
      request: { sessionId: "session-1", includeTerminal: true }, result: { agentRuns: [runSummary("cancelled")] },
      expected: [runtimeRun("cancelled")],
    },
  ];
  for (const specification of cases) {
    const result = specification.invoke();
    const request = await fixture.next();
    assert.equal(request.method, specification.rpc);
    assert.deepEqual(request.params.request, specification.request);
    fixture.respond(request, specification.result);
    assert.deepEqual(await result, specification.expected ?? specification.result);
  }
  fixture.client.close();
  await assert.rejects(session.load(), RpcConnectionClosedError);
});

test("resumeSession loads history and reattaches the existing Run", { timeout: 5000 }, async (t) => {
  const fixture = await connected(t);
  const resuming = fixture.client.resumeSession("session-1");
  const load = await fixture.next();
  assert.equal(load.method, "session/load");
  assert.deepEqual(load.params.request, { sessionId: "session-1" });
  fixture.respond(load, sessionData("session-1", [persistedMessage()]));
  const subscribe = await fixture.next();
  assert.equal(subscribe.method, "_centaeris/session/agent-runs/attach");
  assert.deepEqual(subscribe.params.request, { sessionId: "session-1", viewerId: "sdk-viewer" });
  const subscription = { agentRun: runSummary("running"), viewerId: "sdk-viewer", transitionReason: "viewer_attached", attachedViewerCount: 1 };
  fixture.respond(subscribe, subscription);
  const resumed = await resuming;
  assert.ok(resumed.session instanceof AgentSession);
  assert.equal(resumed.session.id, "session-1");
  assert.deepEqual(resumed.subscription, { ...subscription, agentRun: runtimeRun("running") });
});

test("bound answerNow verifies Run ownership and preserves the Core intervention", { timeout: 5000 }, async (t) => {
  const fixture = await connected(t);
  const opening = fixture.client.openSession("session-1");
  fixture.respond(await fixture.next(), sessionData("session-1"));
  const session = await opening;
  const intervention = { schema: "agent_run.intervention.v1", interventionId: "answer-1", agentRunId: "run-1", kind: "answer_now" };
  await assert.rejects(session.answerNow({ ...intervention, sessionId: "session-2" }), TypeError);
  const mismatched = session.answerNow(intervention);
  const rejected = assert.rejects(mismatched, TypeError);
  const lookup = await fixture.next();
  assert.equal(lookup.method, "_centaeris/session/agent-runs");
  assert.deepEqual(lookup.params.request, { sessionId: "session-1", includeTerminal: true });
  fixture.respond(lookup, { agentRuns: [{ ...runSummary("running"), sessionId: "session-2" }] });
  await rejected;
  const answering = session.answerNow(intervention);
  fixture.respond(await fixture.next(), { agentRuns: [runSummary("running")] });
  const request = await fixture.next();
  assert.equal(request.method, "_centaeris/session/answer_now");
  assert.deepEqual(request.params.request, intervention);
  const receipt = { accepted: true, disposition: "accepted", sessionId: "session-1", agentRunId: "run-1", interventionId: "answer-1" };
  fixture.respond(request, receipt);
  assert.deepEqual(await answering, receipt);
});

test("RPC errors retain code and data without disturbing the next request", { timeout: 5000 }, async (t) => {
  const fixture = await connected(t);
  const rejected = fixture.client.create({ operationId: "conflict", cwd: "/fixture" });
  const assertion = assert.rejects(rejected, (error) => {
    assert.ok(error instanceof RpcError);
    assert.equal(error.code, -32000);
    assert.deepEqual(error.data, { code: "operation_id_conflict" });
    return true;
  });
  const request = await fixture.next();
  fixture.responses.write(`${JSON.stringify({ jsonrpc: "2.0", id: request.id, error: { code: -32000, message: "operation conflict", data: { code: "operation_id_conflict" } } })}\n`);
  await assertion;
  const next = fixture.client.load({ sessionId: "session-1" });
  const nextRequest = await fixture.next();
  fixture.respond(nextRequest, sessionData("session-1"));
  assert.equal((await next).id, "session-1");
});

test("transport EOF rejects every pending request and future calls", { timeout: 5000 }, async (t) => {
  const fixture = await connected(t);
  const create = fixture.client.create({ operationId: "pending-create", cwd: "/fixture" });
  const send = fixture.client.send({ operationId: "pending-prompt", message: "hello" });
  const assertions = [assert.rejects(create, RpcConnectionClosedError), assert.rejects(send, RpcConnectionClosedError)];
  await fixture.next();
  await fixture.next();
  fixture.responses.end();
  await Promise.all(assertions);
  await assert.rejects(fixture.client.load({ sessionId: "session-1" }), RpcConnectionClosedError);
});

test("explicit close rejects pending requests and releases both streams", { timeout: 5000 }, async (t) => {
  const fixture = await connected(t);
  const sending = fixture.client.send({ operationId: "close-prompt", message: "hello" });
  const rejected = assert.rejects(sending, RpcConnectionClosedError);
  await fixture.next();
  fixture.client.close();
  await rejected;
  assert.equal(fixture.requests.destroyed, true);
  assert.equal(fixture.responses.destroyed, true);
});

test("malformed response envelopes close the connection instead of resolving a request", { timeout: 5000 }, async (t) => {
  const fixture = await connected(t);
  const sending = fixture.client.send({ operationId: "bad-response", message: "hello" });
  const rejected = assert.rejects(sending, RpcProtocolError);
  const request = await fixture.next();
  fixture.responses.write(`${JSON.stringify({ jsonrpc: "2.0", id: request.id, result: null, error: { code: -32603, message: "both" } })}\n`);
  await rejected;
});

test("unknown response IDs fail pending requests without misassociation", { timeout: 5000 }, async (t) => {
  const fixture = await connected(t);
  const sending = fixture.client.send({ operationId: "unknown-response", message: "hello" });
  const rejected = assert.rejects(sending, RpcProtocolError);
  await fixture.next();
  fixture.responses.write(`${JSON.stringify({ jsonrpc: "2.0", id: "other-request", result: {} })}\n`);
  await rejected;
});

test("Node JSONL rejects invalid UTF-8 and consumes a valid final frame before EOF", { timeout: 5000 }, async (t) => {
  const fixture = await connected(t);
  const updates = [];
  fixture.client.onUpdate((update) => updates.push(update));
  fixture.responses.end(JSON.stringify({ jsonrpc: "2.0", method: "session/update", params: { sessionId: "session-1" } }));
  await setImmediate();
  assert.equal(updates[0].sessionId, "session-1");
  const malformed = await connected(t);
  const sending = malformed.client.send({ operationId: "bad-encoding", message: "hello" });
  const rejected = assert.rejects(sending, RpcProtocolError);
  await malformed.next();
  malformed.responses.write(Buffer.from([0xff, 0x0a]));
  await rejected;
});

test("send failures settle requests and unsupported descriptors fail initialization", { timeout: 5000 }, async () => {
  let receiver;
  let requests = 0;
  let closes = 0;
  const transport = {
    listen(next) { receiver = next; return () => {}; },
    send(frame) {
      const request = JSON.parse(frame);
      requests++;
      if (request.method === "initialize") {
        receiver.message(JSON.stringify({ jsonrpc: "2.0", id: request.id, result: descriptor }));
        return;
      }
      return Promise.reject(new Error("pipe failed"));
    },
    close() { closes++; },
  };
  const client = await SessionClient.connect(transport, { viewerId: "send-failure" });
  await assert.rejects(client.send({ operationId: "failed-send", message: "hello" }), RpcConnectionClosedError);
  await assert.rejects(client.load({ sessionId: "session-1" }), RpcConnectionClosedError);
  assert.equal(requests, 2);
  client.close();
  assert.equal(closes, 1);

  const fixture = wireFixture();
  const connection = SessionClient.connect(fixture.transport, { viewerId: "wrong-protocol" });
  const rejected = assert.rejects(connection, RpcProtocolError);
  fixture.respond(await fixture.next(), { ...descriptor, protocolVersion: 2 });
  await rejected;
  assert.equal(fixture.requests.destroyed, true);
});

test("Session methods reject missing, wrong-typed, extra and legacy result fields", { timeout: 5000 }, async (t) => {
  const cases = [
    {
      method: "create",
      request: { operationId: "malformed-create", cwd: "/fixture" },
      result: sessionItem("session-1"),
      missing: "cwd",
      wrong: { messageCount: "0" },
      renamed: ["updatedAt", "updated_at"],
    },
    {
      method: "load",
      request: { sessionId: "session-1" },
      result: sessionData("session-1"),
      missing: "messages",
      wrong: { messages: {} },
      renamed: ["createdAt", "created_at"],
    },
    {
      method: "send",
      request: { operationId: "malformed-prompt", message: "hello" },
      result: { sessionId: "session-1", agentRunId: "run-1", turnId: "turn-1", streamItems: [] },
      missing: "turnId",
      wrong: { streamItems: {} },
      renamed: ["agentRunId", "agent_run_id"],
    },
    {
      method: "subscribe",
      request: { sessionId: "session-1" },
      result: { agentRun: runSummary("running"), viewerId: "sdk-viewer", transitionReason: "viewer_attached", attachedViewerCount: 1 },
      missing: "agentRun",
      wrong: { attachedViewerCount: -1 },
      renamed: ["viewerId", "viewer_id"],
    },
    {
      method: "cancel",
      request: { agentRunId: "run-1" },
      result: { agentRun: runSummary("running"), cancelAccepted: true },
      missing: "cancelAccepted",
      wrong: { cancelAccepted: "true" },
      renamed: ["agentRun", "agent_run"],
    },
    {
      method: "supplement",
      request: { sessionId: "session-1", agentRunId: "run-1", message: "include tests" },
      result: { accepted: true, sessionId: "session-1", agentRunId: "run-1", queuedCount: 1 },
      missing: "queuedCount",
      wrong: { queuedCount: -1 },
      renamed: ["agentRunId", "agent_run_id"],
    },
    {
      method: "answerNow",
      request: { schema: "agent_run.intervention.v1", interventionId: "answer-1", agentRunId: "run-1", kind: "answer_now" },
      result: { accepted: true, disposition: "accepted", sessionId: "session-1", agentRunId: "run-1", interventionId: "answer-1" },
      missing: "interventionId",
      wrong: { disposition: "unknown" },
      renamed: ["interventionId", "intervention_id"],
    },
    {
      method: "answerQuestion",
      request: { sessionId: "session-1", questionId: "question-1", answers: ["first"] },
      result: { sessionId: "session-1", agentRunId: "run-1", turnId: "turn-1", streamItems: [] },
      missing: "turnId",
      wrong: { streamItems: {} },
      renamed: ["agentRunId", "agent_run_id"],
    },
    {
      method: "compactContext",
      request: { sessionId: "session-1" },
      result: { sessionId: "session-1", compacted: true },
      missing: "compacted",
      wrong: { compacted: "true" },
      renamed: ["sessionId", "session_id"],
    },
    {
      method: "listRuns",
      request: { sessionId: "session-1", includeTerminal: true },
      result: { agentRuns: [runSummary("running")] },
      missing: "agentRuns",
      wrong: { agentRuns: {} },
      renamed: ["agentRuns", "agent_runs"],
    },
  ];
  for (const specification of cases) {
    const missing = structuredClone(specification.result);
    delete missing[specification.missing];
    const legacy = structuredClone(specification.result);
    const [currentName, oldName] = specification.renamed;
    legacy[oldName] = legacy[currentName];
    delete legacy[currentName];
    const malformed = {
      missing,
      wrongType: { ...specification.result, ...specification.wrong },
      unknownField: { ...specification.result, unexpected: true },
      legacy,
    };
    for (const [kind, value] of Object.entries(malformed)) {
      await t.test(`${specification.method} ${kind}`, async (t) => {
        const fixture = await connected(t);
        const response = fixture.client[specification.method](specification.request);
        const rejected = assert.rejects(response, RpcProtocolError);
        fixture.respond(await fixture.next(), value);
        await rejected;
        assert.equal(fixture.responses.destroyed, true);
      });
    }
  }
});

test("nested response DTOs reject invalid fields and serialized null for omitted string options", { timeout: 5000 }, async (t) => {
  const malformed = [
    { method: "load", result: sessionData("session-1", [{ ...persistedMessage(), status: null }]) },
    { method: "load", result: sessionData("session-1", [{ ...persistedMessage(), turn_id: "turn-1" }]) },
    { method: "subscribe", result: { agentRun: { ...runSummary("running"), completedAtMs: "2" }, viewerId: "sdk-viewer", transitionReason: "viewer_attached", attachedViewerCount: 1 } },
    { method: "cancel", result: { agentRun: { ...runSummary("running"), agent_run_id: "run-1" }, cancelAccepted: true } },
    { method: "listRuns", result: { agentRuns: [{ ...runSummary("running"), completedAtMs: "2" }] } },
  ];
  for (const [index, specification] of malformed.entries()) {
    await t.test(`${specification.method} nested payload ${index}`, async (t) => {
      const fixture = await connected(t);
      const response = fixture.client[specification.method]({ sessionId: "session-1" });
      const rejected = assert.rejects(response, RpcProtocolError);
      fixture.respond(await fixture.next(), specification.result);
      await rejected;
    });
  }
});

test("response decoding preserves serde optional fields, nullable Runs and opaque JSON", { timeout: 5000 }, async (t) => {
  const fixture = await connected(t);
  const loaded = sessionData("session-1", [
    persistedMessage(),
    { ...persistedMessage(), id: "message-2", status: "done", agentRunId: "run-1", imageData: null },
  ]);
  const loading = fixture.client.load({ sessionId: "session-1" });
  fixture.respond(await fixture.next(), loaded);
  assert.deepEqual(await loading, loaded);

  const receipt = {
    sessionId: "session-1", agentRunId: "run-1", turnId: "turn-1",
    streamItems: [null, 1, { schema: "host-owned-content", arbitrary: [true, "value"] }],
  };
  const sending = fixture.client.send({ operationId: "opaque-result", message: "hello" });
  fixture.respond(await fixture.next(), receipt);
  assert.deepEqual(await sending, receipt);

  const attachment = { agentRun: null, viewerId: "sdk-viewer", transitionReason: "viewer_attached", attachedViewerCount: 1 };
  const attaching = fixture.client.subscribe({ sessionId: "empty-session" });
  fixture.respond(await fixture.next(), attachment);
  assert.deepEqual(await attaching, attachment);

  const cancellation = { agentRun: { ...runSummary("running"), watchdog: { arbitrary: [null, "opaque"] } }, cancelAccepted: true };
  const cancelling = fixture.client.cancel({ agentRunId: "run-1" });
  fixture.respond(await fixture.next(), cancellation);
  assert.deepEqual(await cancelling, cancellation);
  const emptyCancellation = fixture.client.cancel({ sessionId: "empty-session" });
  fixture.respond(await fixture.next(), { agentRun: null, cancelAccepted: false });
  assert.deepEqual(await emptyCancellation, { agentRun: null, cancelAccepted: false });
});

test("initialize rejects missing, wrong-typed, unknown and legacy descriptor fields", { timeout: 5000 }, async (t) => {
  const missing = { ...descriptor };
  delete missing.storeId;
  const legacy = { ...descriptor, protocol_version: descriptor.protocolVersion };
  delete legacy.protocolVersion;
  const malformed = {
    missing,
    wrongArrayElement: { ...descriptor, events: ["session/update", 1] },
    wrongVersionType: { ...descriptor, layoutSchemaVersion: "1" },
    unknownField: { ...descriptor, unexpected: true },
    legacy,
  };
  for (const [kind, value] of Object.entries(malformed)) {
    await t.test(kind, async () => {
      const fixture = wireFixture();
      const connecting = SessionClient.connect(fixture.transport, { viewerId: "strict-initialize" });
      const rejected = assert.rejects(connecting, RpcProtocolError);
      fixture.respond(await fixture.next(), value);
      await rejected;
      assert.equal(fixture.requests.destroyed, true);
    });
  }
});
