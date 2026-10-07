import assert from "node:assert/strict";
import test from "node:test";
import { OperationClient, acceptedConversationReviewLink, consumeReviewedOperation, loadAcceptedConversation } from "../../src/chat/operationReceipts.ts";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { ApiError } from "../../src/api.ts";

const scope = { userId: "user", workspaceId: "workspace", command: "submitMessage" };
const input = { text: "private prompt", modelConfigRef: "model", agentId: "agent" };
const accepted = (operationId) => ({ operationId, command: "submitMessage", status: "accepted", sessionId: "session", agentRunId: "run", turnId: "turn" });
function storage() {
  const data = new Map();
  return { getItem: (key) => data.get(key) ?? null, setItem: (key, value) => data.set(key, value), removeItem: (key) => data.delete(key), data };
}
function client(store, request) { return new OperationClient(scope, { storage: store, request }); }

test("a lost POST response recovers the accepted receipt without another submission", async () => {
  const store = storage(); const calls = []; let id;
  const operation = client(store, async (path, options) => {
    calls.push(options?.method || "GET");
    if (options) { id = JSON.parse(options.body).operationId; throw new TypeError("response lost"); }
    assert.ok(path.endsWith(id));
    return accepted(id);
  });
  assert.deepEqual(await operation.submit("/submit", input), accepted(id));
  assert.deepEqual(calls, ["POST", "GET"]);
  assert.equal(operation.pending().operationId, id);
});

test("silent receipt recovery preserves an authorization failure instead of hiding it", async () => {
  const forbidden = new ApiError("forbidden", 403);
  const operation = client(storage(), async (_path, options) => {
    if (options) throw new TypeError("response lost");
    throw forbidden;
  });
  await assert.rejects(operation.submit("/submit", input), error => error === forbidden);
  assert.ok(operation.pending(), "unconfirmed identity is retained without another POST");
});

test("lost response and reload keep the operation identity without persisting prompt or file bytes", async () => {
  const store = storage();
  let sent;
  const first = client(store, async (_path, options) => { sent = JSON.parse(options.body); throw new TypeError("network lost"); });
  await assert.rejects(first.submit("/submit", input));
  assert.ok(sent.operationId);
  assert.doesNotMatch([...store.data.values()].join(), /private prompt/);
  let calls = 0;
  const reloaded = client(store, async (path, options) => {
    calls++;
    assert.equal(options, undefined);
    assert.ok(path.endsWith(sent.operationId));
    return accepted(sent.operationId);
  });
  assert.deepEqual(await reloaded.submit("/submit", input), accepted(sent.operationId));
  assert.equal(calls, 1);
  assert.ok(reloaded.pending());
  reloaded.complete(sent.operationId);
  assert.equal(reloaded.pending(), null);
});

test("lookup absence replays the same operation ID and changed input cannot silently start a second task", async () => {
  const store = storage();
  const original = client(store, async () => { throw new TypeError("network"); });
  await assert.rejects(original.submit("/submit", input));
  const id = original.pending().operationId;
  const posts = [];
  const retry = client(store, async (_path, options) => {
    if (!options) throw new ApiError("operation_not_found", 404);
    posts.push(JSON.parse(options.body));
    return accepted(id);
  });
  await assert.rejects(retry.submit("/submit", { ...input, text: "changed" }), /operation_pending_input_changed/);
  assert.equal(posts.length, 0);
  await retry.submit("/submit", input);
  assert.equal(posts[0].operationId, id);
});

test("accepted receipt survives downstream failures and malformed responses cannot consume identity", async () => {
  const store = storage();
  const api = client(store, async () => ({ status: "queued" }));
  await assert.rejects(api.submit("/submit", input), /operation_receipt_invalid/);
  const id = api.pending().operationId;
  const recovered = client(store, async () => accepted(id));
  await recovered.recover();
  const again = client(store, async (_path, options) => { assert.equal(options, undefined); return accepted(id); });
  assert.deepEqual(await again.submit("/submit", input), accepted(id));
});

test("a rejected retry cannot erase an earlier uncertain request identity", async () => {
  const store = storage();
  const first = client(store, async () => { throw new TypeError("network"); });
  await assert.rejects(first.submit("/submit", input));
  const id = first.pending().operationId;
  const retry = client(store, async (_path, options) => {
    if (!options) throw new ApiError("operation_not_found", 404);
    throw new ApiError("model_not_found", 400);
  });
  await assert.rejects(retry.submit("/submit", input), /model_not_found/);
  assert.equal(retry.pending().operationId, id);
});

test("explicitly handling an unavailable accepted resource frees the local slot without submitting", async () => {
  const store = storage();
  const first = client(store, async () => { throw new TypeError("network"); });
  await assert.rejects(first.submit("/submit", input));
  const unavailable = client(store, async (_path, options) => { assert.equal(options, undefined); throw new ApiError("operation_resource_unavailable", 410); });
  await assert.rejects(unavailable.recover(), /operation_resource_unavailable/);
  unavailable.complete(unavailable.pending().operationId);
  assert.equal(unavailable.pending(), null);
});

test("same-name same-length different upload bytes cannot reuse the pending operation", async () => {
  const store = storage();
  const api = client(store, async () => { throw new TypeError("network"); });
  await assert.rejects(api.submit("/submit", input, [new File(["AAA"], "same.txt")]));
  await assert.rejects(api.submit("/submit", input, [new File(["BBB"], "same.txt")]), /operation_pending_input_changed/);
  assert.doesNotMatch([...store.data.values()].join(), /AAA|BBB|private prompt/);
});

test("recovery only reads, respects caller scope, and does not clear missing or unavailable receipts", async () => {
  const store = storage();
  const api = client(store, async () => { throw new TypeError("network"); });
  await assert.rejects(api.submit("/submit", input));
  const another = new OperationClient({ ...scope, userId: "other" }, { storage: store, request: async () => { throw Error("unexpected"); } });
  assert.equal(another.pending(), null);
  for (const [status, code] of [[404, "operation_not_found"], [410, "operation_resource_unavailable"]]) {
    const checking = client(store, async (_path, options) => { assert.equal(options, undefined); throw new ApiError(code, status); });
    await assert.rejects(checking.recover(), { message: code });
    assert.ok(checking.pending());
  }
});

test("opening an old acceptance follows a later active run, and terminal sessions need no stream", async () => {
  for (const current of [{ agentRunId: "later-run", status: "running", streamCursor: "42" }, null]) {
    let displayed;
    const resume = await loadAcceptedConversation("session", {
      transport: {
        loadTail: async () => ({ projectionVersion: "v1", projectionGeneration: "g1", sourceHighWater: "40" }),
        loadActiveAgentRun: async () => ({ agentRun: current }),
      },
      store: { openTail: () => 3 }, setActive: (value) => { displayed = value; }, isCurrent: () => true,
    }, new AbortController().signal);
    assert.deepEqual(displayed, current);
    assert.equal(resume?.agentRunId ?? null, current?.agentRunId ?? null);
    assert.equal(resume?.cursor ?? null, current?.streamCursor ?? null);
  }
});

test("navigating during receipt recovery cannot replace the current conversation", async () => {
  await assert.rejects(loadAcceptedConversation("old-session", {
    transport: {
      loadTail: async () => ({ projectionVersion: "v1", projectionGeneration: "g1", sourceHighWater: "40" }),
      loadActiveAgentRun: async () => ({ agentRun: null }),
    },
    store: { openTail: () => assert.fail("stale projection") },
    setActive: () => assert.fail("stale active run"), isCurrent: () => false,
  }, new AbortController().signal), { name: "AbortError" });
});

test("explicit input correction after absent receipt keeps the original operation identity", async () => {
  const store = storage();
  const first = client(store, async () => { throw new TypeError("network"); });
  await assert.rejects(first.submit("/submit", input));
  const id = first.pending().operationId;
  const retry = client(store, async (_path, options) => {
    if (!options) throw new ApiError("operation_not_found", 404);
    const posted = JSON.parse(options.body);
    assert.equal(posted.operationId, id);
    assert.equal(posted.text, "corrected");
    return accepted(id);
  });
  assert.deepEqual(await retry.submit("/submit", { ...input, text: "corrected" }, [], [], true), accepted(id));
});

test("an original request winning the corrected-input race recovers its receipt without claiming the correction was accepted", async () => {
  const store = storage();
  const first = client(store, async () => { throw new TypeError("network"); });
  await assert.rejects(first.submit("/submit", input));
  const id = first.pending().operationId;
  let calls = 0;
  const retry = client(store, async (_path, options) => {
    calls++;
    if (calls === 1) throw new ApiError("operation_not_found", 404);
    if (calls === 2) { assert.equal(JSON.parse(options.body).operationId, id); throw new ApiError("operation_conflict", 409); }
    assert.equal(options, undefined);
    return accepted(id);
  });
  await assert.rejects(retry.submit("/submit", { ...input, text: "corrected" }, [], [], true), /operation_input_acceptance_unconfirmed/);
  assert.deepEqual(retry.pending().receipt, accepted(id));
  assert.equal(calls, 3);
});

test("a receipt found after a corrected request loses its response does not prove which input was accepted", async () => {
  const store = storage();
  const first = client(store, async () => { throw new TypeError("network"); });
  await assert.rejects(first.submit("/submit", input));
  const id = first.pending().operationId;
  const correction = client(store, async (_path, options) => {
    if (!options) throw new ApiError("operation_not_found", 404);
    throw new TypeError("lost corrected response");
  });
  await assert.rejects(correction.submit("/submit", { ...input, text: "corrected" }, [], [], true));
  const reloaded = client(store, async (_path, options) => { assert.equal(options, undefined); return accepted(id); });
  await assert.rejects(reloaded.submit("/submit", { ...input, text: "corrected" }), /operation_input_acceptance_unconfirmed/);
  assert.deepEqual(reloaded.pending().receipt, accepted(id));
});

test("late receipt lookup cannot resurrect a consumed operation over a new submission", async () => {
  const store = storage();
  const first = client(store, async () => { throw new TypeError("network"); });
  await assert.rejects(first.submit("/submit", input));
  const oldId = first.pending().operationId;
  let release;
  const late = client(store, () => new Promise((resolve) => { release = resolve; }));
  const checking = late.recover();
  first.complete(oldId);
  await assert.rejects(first.submit("/submit", { ...input, text: "new intent" }));
  const newId = first.pending().operationId;
  release(accepted(oldId));
  await assert.rejects(checking, /operation_result_stale/);
  assert.equal(first.pending().operationId, newId);
});

test("late successful POST cannot overwrite a new operation after its receipt was consumed elsewhere", async () => {
  const store = storage();
  let release;
  let started;
  const sent = new Promise((resolve) => { started = resolve; });
  const late = client(store, (_path, options) => new Promise((resolve) => { release = resolve; started(JSON.parse(options.body).operationId); }));
  const submitting = late.submit("/submit", input);
  const oldId = await sent;
  const other = client(store, async (_path, options) => {
    if (options) throw new TypeError("new request unknown");
    return accepted(oldId);
  });
  await other.recover();
  other.complete(oldId);
  await assert.rejects(other.submit("/submit", { ...input, text: "new intent" }));
  const newId = other.pending().operationId;
  release(accepted(oldId));
  await assert.rejects(submitting, /operation_result_stale/);
  assert.equal(other.pending().operationId, newId);
});

test("late original rejection cannot clear the same ID after corrected input was accepted", async () => {
  const store = storage();
  let rejectOriginal;
  let started;
  const sent = new Promise((resolve) => { started = resolve; });
  const original = client(store, (_path, options) => new Promise((_resolve, reject) => { rejectOriginal = reject; started(JSON.parse(options.body).operationId); }));
  const submitting = original.submit("/submit", input);
  const id = await sent;
  const correction = client(store, async (_path, options) => {
    if (!options) throw new ApiError("operation_not_found", 404);
    return accepted(id);
  });
  await correction.submit("/submit", { ...input, text: "corrected" }, [], [], true);
  rejectOriginal(new ApiError("model_not_found", 400));
  await assert.rejects(submitting);
  assert.deepEqual(correction.pending().receipt, accepted(id));
});

test("a late lookup cannot roll back corrected input under the same operation ID", async () => {
  const store = storage();
  const first = client(store, async () => { throw new TypeError("network"); });
  await assert.rejects(first.submit("/submit", input));
  const id = first.pending().operationId;
  let release;
  const oldLookup = client(store, () => new Promise((resolve) => { release = resolve; }));
  const checking = oldLookup.recover();
  const correcting = client(store, async (_path, options) => {
    if (!options) throw new ApiError("operation_not_found", 404);
    throw new TypeError("corrected response lost");
  });
  await assert.rejects(correcting.submit("/submit", { ...input, text: "corrected" }, [], [], true));
  const corrected = correcting.pending();
  release(accepted(id));
  await assert.rejects(checking, /operation_result_stale/);
  assert.deepEqual(correcting.pending(), corrected);
});

test("reviewing ambiguous input uses a real new-tab link and consumes metadata only after receipt and Session authorization", async () => {
  const store = storage();
  const first = client(store, async () => { throw new TypeError("network"); });
  await assert.rejects(first.submit("/submit", input));
  const id = first.pending().operationId;
  const calls = [];
  const checking = client(store, async (_path, options) => { assert.equal(options, undefined); calls.push("receipt"); return accepted(id); });
  const link = acceptedConversationReviewLink("workspace", { id: "session", agentId: "another-agent" });
  const html = renderToStaticMarkup(createElement("a", link, "Review"));
  assert.match(html, /href="\/w\/workspace\/agents\/another-agent\?sessionId=session"/);
  assert.match(html, /target="_blank"/);
  assert.match(html, /rel="noopener"/);
  await assert.rejects(consumeReviewedOperation(checking, id, async () => { throw new ApiError("session_not_found", 404); }));
  assert.ok(checking.pending());
  await consumeReviewedOperation(checking, id, async (sessionId) => {
    assert.equal(sessionId, "session"); calls.push("session"); return { id: "session", agentId: "another-agent" };
  });
  assert.deepEqual(calls, ["receipt", "receipt", "session"]);
  assert.equal(checking.pending(), null);
});
