import assert from "node:assert/strict";
import { test } from "node:test";
import { ApiError } from "../../src/api.ts";
import { OperationClient } from "../../src/chat/operationReceipts.ts";
import { SubmissionControl } from "../../src/chat/submissionControl.ts";

const scope = { userId: "user", workspaceId: "workspace", command: "submitMessage" as const };
const input = { text: "synthetic private message", modelConfigRef: "model", agentId: "agent" };
const target = { sessionId: "session", agentRunId: "run" };
const accepted = operationId => ({ operationId, command: "submitMessage", status: "accepted", ...target, turnId: "turn" });
const requested = { agentRunId: "run", status: "running", disposition: "requested" };
const terminal = { agentRunId: "run", status: "cancelled", disposition: "terminal" };
function storage() {
  const data = new Map<string, string>();
  return { data, getItem: key => data.get(key) ?? null, setItem: (key, value) => data.set(key, value), removeItem: key => data.delete(key) };
}
function deferred<T = unknown>() { let resolve!: (value: T) => void; let reject!: (error: unknown) => void; const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }
async function until(check) { for (let i = 0; i < 100; i++) { if (check()) return; await new Promise(resolve => setImmediate(resolve)); } assert.fail("controlled request did not reach boundary"); }
function setup(request, cancel = async () => requested, store = storage()) {
  const operations = new OperationClient(scope, { storage: store, request });
  const control = new SubmissionControl({ operations, scope, storage: store, cancel });
  return { operations, control, store };
}
const submit = f => f.control.submit(hooks => f.operations.submit("/messages", input, [], [], false, hooks));

test("Stop is accepted synchronously during preparation and prevents a first POST", async () => {
  let requests = 0; const f = setup(async () => { requests++; throw Error("unexpected request"); });
  const updates = []; const unsubscribe = f.control.subscribe(() => updates.push(f.control.getSnapshot()));
  f.control.begin(); assert.equal(f.control.getSnapshot().phase, "preparing");
  const sending = submit(f); await f.control.stop();
  await assert.rejects(sending, /operation_dispatch_stopped/);
  assert.equal(requests, 0); assert.equal(f.operations.pending(), null);
  assert.equal(f.control.getSnapshot().feedback, "stoppedBeforeDispatch");
  assert.ok(updates.some(value => value.feedback === "stopRecorded")); unsubscribe();
});

test("Stop before the receipt targets that original acceptance and duplicate cancellation shares one promise", async () => {
  const receipt = deferred(); const cancellation = deferred(); const calls = []; const cancelled = [];
  const f = setup(async (_path, options) => { calls.push(options?.method || "GET"); return receipt.promise; }, async value => { cancelled.push(value); return cancellation.promise; });
  f.control.begin(); const sending = submit(f); await until(() => calls.length === 1);
  await f.control.stop(); assert.equal(f.control.getSnapshot().feedback, "stopRecorded");
  const operationId = f.operations.pending()!.operationId; receipt.resolve(accepted(operationId)); await sending;
  await until(() => cancelled.length === 1);
  const first = f.control.stop(), second = f.control.stop(); assert.equal(first, second);
  assert.equal(f.control.getSnapshot().feedback, "stopInProgress");
  cancellation.resolve(requested); await first;
  assert.deepEqual(cancelled, [target]); assert.deepEqual(calls, ["POST"]);
  assert.equal(f.control.getSnapshot().phase, "stopRequested");
  assert.deepEqual(f.control.getSnapshot().target, target);
  assert.equal(f.control.getSnapshot().terminal, null);
});

test("unknown acceptance retains the stop across reload, never replays POST, and rejects a new attempt", async () => {
  let recoverable = false; const calls = []; const cancelled = [];
  const store = storage(); let operationId;
  const request = async (path, options) => {
    calls.push([path, options?.method || "GET"]);
    if (options) { operationId = JSON.parse(options.body).operationId; throw new TypeError("lost response"); }
    if (!recoverable) throw new ApiError("operation_not_found", 404);
    return accepted(operationId);
  };
  const f = setup(request, async value => { cancelled.push(value); return requested; }, store);
  f.control.begin(); await assert.rejects(submit(f), /lost response/); await f.control.stop();
  assert.equal(f.control.getSnapshot().phase, "unknown");
  assert.throws(() => f.control.begin(), /submission_stop_pending/);
  assert.equal(f.control.getSnapshot().feedback, "pendingStop");
  const reloaded = setup(request, async value => { cancelled.push(value); return requested; }, store);
  assert.equal(reloaded.control.getSnapshot().stopRequested, true);
  assert.equal(reloaded.control.getSnapshot().operationId, operationId);
  recoverable = true; await reloaded.control.recover();
  assert.deepEqual(cancelled, [target]); assert.equal(reloaded.control.getSnapshot().phase, "stopRequested");
  assert.equal(calls.filter(([, method]) => method === "POST").length, 1);
  assert.equal(reloaded.operations.pending()!.operationId, operationId);
  const metadata = [...store.data.entries()].find(([key]) => key.startsWith("centaeris.submissionControl.v1:"))![1];
  assert.doesNotMatch(metadata, /synthetic private message|modelConfigRef|agentId/);
});

test("persisted stop metadata is strictly versioned and transport failure remains unknown", async () => {
  const store = storage();
  const f = setup(async () => { throw new ApiError("forbidden", 403); }, async () => { throw new TypeError("network unavailable"); }, store);
  f.control.setTarget(target); await f.control.stop();
  assert.equal(f.control.getSnapshot().phase, "unknown"); assert.equal(f.control.getSnapshot().error.message, "network unavailable");
  assert.equal(f.control.getSnapshot().terminal, null);
  const entry = [...store.data.entries()].find(([key]) => key.startsWith("centaeris.submissionControl.v1:"))!;
  const value = JSON.parse(entry[1]); store.setItem(entry[0], JSON.stringify({ ...value, unexpected: true }));
  assert.throws(() => setup(async () => null, undefined, store), /submission_control_storage_invalid/);
  store.setItem(entry[0], JSON.stringify({ ...value, schema: "web.submission_control.v99" }));
  assert.throws(() => setup(async () => null, undefined, store), /submission_control_storage_invalid/);
});

test("matching terminal before receipt is irreversible and prevents cancellation of a finished Run", async () => {
  const receipt = deferred(); let cancelled = 0;
  const f = setup(async () => receipt.promise, async () => { cancelled++; return terminal; });
  f.control.begin(); const sending = submit(f); await until(() => Boolean(f.operations.pending()));
  await f.control.stop(); f.control.observeTerminal({ ...target, status: "completed", completedAtMs: 123 });
  receipt.resolve(accepted(f.operations.pending()!.operationId)); await sending;
  assert.equal(f.control.getSnapshot().phase, "terminal"); assert.equal(f.control.getSnapshot().terminal.status, "completed");
  assert.deepEqual(f.control.getSnapshot().target, target); assert.equal(cancelled, 0);
  assert.equal(f.control.setTarget(target), false);
});

test("authority can follow a later current Run but cannot move an outstanding stop to it", async () => {
  const f = setup(async (_path, options) => accepted(JSON.parse(options.body).operationId));
  f.control.begin(); await submit(f);
  const later = { sessionId: "session", agentRunId: "later-run" };
  assert.equal(f.control.setTarget(later), true); f.control.observeTerminal({ ...target, status: "cancelled" });
  assert.deepEqual(f.control.getSnapshot().target, later); assert.equal(f.control.getSnapshot().phase, "accepted");
  assert.equal(f.control.setTarget(null), false); assert.deepEqual(f.control.getSnapshot().target, later);
  const stopped = setup(async () => null);
  stopped.control.setTarget(target); await stopped.control.stop();
  assert.equal(stopped.control.setTarget(later), false); assert.deepEqual(stopped.control.getSnapshot().target, target);
});

test("late cancellation cannot overwrite a new attempt after matching terminal was observed", async () => {
  const cancellation = deferred(); const f = setup(async () => null, async () => cancellation.promise);
  f.control.setTarget(target); const stopping = f.control.stop();
  f.control.observeTerminal({ ...target, status: "completed" }); f.control.begin();
  const next = f.control.getSnapshot(); cancellation.resolve(terminal); await stopping;
  assert.equal(f.control.getSnapshot(), next); assert.equal(next.phase, "preparing");
});

test("a terminal stop remains terminal after reload and a new attempt explicitly consumes it", async () => {
  const store = storage(); const f = setup(async () => null, async () => terminal, store);
  f.control.setTarget(target); await f.control.stop();
  const reloaded = setup(async () => { throw Error("unexpected request"); }, async () => { throw Error("unexpected cancellation"); }, store);
  assert.equal(reloaded.control.getSnapshot().phase, "terminal");
  await reloaded.control.stop(); assert.equal(reloaded.control.getSnapshot().phase, "terminal");
  reloaded.control.begin(); assert.equal(reloaded.control.getSnapshot().phase, "preparing");
  assert.equal([...store.data.keys()].some(key => key.startsWith("centaeris.submissionControl.v1:")), false);
});

test("stopping from the dispatch notification still settles as undispatched when no POST occurs", async () => {
  let requests = 0; const f = setup(async () => { requests++; throw Error("unexpected POST"); });
  let requestedStop = false;
  f.control.subscribe(() => { if (f.control.getSnapshot().phase === "submitting" && !requestedStop) { requestedStop = true; void f.control.stop(); } });
  f.control.begin(); await assert.rejects(submit(f), /operation_dispatch_stopped/);
  assert.equal(requests, 0); assert.equal(f.control.getSnapshot().phase, "idle");
  assert.equal(f.control.getSnapshot().feedback, "stoppedBeforeDispatch");
});

for (const status of [400, 422, 429]) test(`a confirmed first-request ${status} rejection releases its stop without claiming a terminal Run`, async () => {
  const response = deferred(); const f = setup(async () => response.promise);
  f.control.begin(); const sending = submit(f); await until(() => Boolean(f.operations.pending()));
  await f.control.stop(); response.reject(new ApiError("request_rejected", status));
  await assert.rejects(sending, /request_rejected/);
  assert.equal(f.operations.pending(), null); assert.equal(f.control.getSnapshot().stopRequested, false);
  assert.equal(f.control.getSnapshot().phase, "idle"); assert.equal(f.control.getSnapshot().terminal, null);
});

test("a current Run may be adopted after presentation has consumed an old pending receipt", async () => {
  const f = setup(async () => { throw new TypeError("offline"); });
  await assert.rejects(f.operations.submit("/messages", input));
  const restored = new SubmissionControl({ operations: f.operations, scope, storage: f.store, cancel: async () => requested });
  f.operations.complete(f.operations.pending()!.operationId);
  assert.equal(restored.setTarget(target), true); assert.deepEqual(restored.getSnapshot().target, target);
});

test("separate current-Run controls preserve both pending cancellations and restore only their own target", async () => {
  const store = storage(); const firstResult = deferred(); const secondResult = deferred();
  const other = { sessionId: "other-session", agentRunId: "other-run" }; const cancelled = [];
  const create = (identity, cancel) => new SubmissionControl({ operations: { pending: () => null, recover: async () => null },
    scope: { ...scope, controlId: JSON.stringify([identity.sessionId, identity.agentRunId]) }, storage: store, cancel });
  const first = create(target, async value => { cancelled.push(value); return firstResult.promise; });
  const second = create(other, async value => { cancelled.push(value); return secondResult.promise; });
  first.setTarget(target); const stoppingFirst = first.stop();
  second.setTarget(other); const stoppingSecond = second.stop();
  await until(() => cancelled.length === 2);
  assert.deepEqual(cancelled, [target, other]); assert.equal(first.getSnapshot().phase, "cancelling");
  assert.equal(second.getSnapshot().phase, "cancelling");
  const restoredFirst = create(target, async value => { assert.deepEqual(value, target); return terminal; });
  const restoredSecond = create(other, async value => { assert.deepEqual(value, other); return { ...terminal, agentRunId: other.agentRunId }; });
  assert.deepEqual(restoredFirst.getSnapshot().target, target); assert.deepEqual(restoredSecond.getSnapshot().target, other);
  await restoredFirst.recover(); await restoredSecond.recover();
  firstResult.resolve(terminal); secondResult.resolve({ ...terminal, agentRunId: other.agentRunId });
  await Promise.all([stoppingFirst, stoppingSecond]);
  assert.equal(first.getSnapshot().phase, "terminal"); assert.equal(second.getSnapshot().phase, "terminal");
  assert.deepEqual(first.getSnapshot().target, target); assert.deepEqual(second.getSnapshot().target, other);
});

test("a stale Stop click after local preparation was stopped cannot create a phantom pending stop", async () => {
  const f = setup(async () => { throw Error("unexpected request"); });
  f.control.begin(); const sending = submit(f); await f.control.stop(); await assert.rejects(sending, /operation_dispatch_stopped/);
  await f.control.stop(); assert.equal(f.control.getSnapshot().phase, "idle"); assert.equal(f.control.getSnapshot().stopRequested, false);
  assert.doesNotThrow(() => f.control.begin());
});

for (const status of [401, 403, 404, 410]) test(`a definite cancellation ${status} rejection is explicit and allows user retry after access changes`, async () => {
  let denied = true; let calls = 0;
  const f = setup(async () => null, async () => { calls++; if (denied) throw new ApiError("agent_run_not_found", status); return requested; });
  f.control.setTarget(target); await f.control.stop();
  assert.equal(f.control.getSnapshot().phase, "rejected"); assert.equal(f.control.getSnapshot().feedback, "stopRejected");
  assert.equal(f.control.getSnapshot().terminal, null); assert.equal(f.control.getSnapshot().stopRequested, false);
  assert.deepEqual(f.control.getSnapshot().target, target); assert.equal(calls, 1);
  denied = false; await f.control.stop(); assert.equal(calls, 2); assert.equal(f.control.getSnapshot().phase, "stopRequested");
});

test("a first explicit resource rejection does not block a later submission attempt", async () => {
  const f = setup(async () => null, async () => { throw new ApiError("agent_run_not_found", 404); });
  f.control.setTarget(target); await f.control.stop();
  assert.doesNotThrow(() => f.control.begin()); assert.equal(f.control.getSnapshot().phase, "preparing");
});

test("receipt authorization rejection is explicit while its original unknown submission identity remains", async () => {
  let recoverDenied = false; let posts = 0;
  const f = setup(async (_path, options) => {
    if (options) { posts++; throw new TypeError("lost submission response"); }
    if (recoverDenied) throw new ApiError("forbidden", 403);
    throw new ApiError("operation_not_found", 404);
  });
  f.control.begin(); await assert.rejects(submit(f), /lost submission response/);
  const operationId = f.operations.pending()!.operationId; recoverDenied = true;
  await f.control.stop(); assert.equal(f.control.getSnapshot().phase, "rejected");
  assert.equal(f.control.getSnapshot().feedback, "stopRejected"); assert.equal(f.control.getSnapshot().terminal, null);
  assert.equal(f.operations.pending()!.operationId, operationId); assert.equal(posts, 1);
  assert.doesNotThrow(() => f.control.begin());
});

test("a rejected retry cannot erase an earlier unknown cancellation when another attempt begins", async () => {
  let calls = 0; const f = setup(async () => null, async () => {
    calls++; if (calls === 1) throw new TypeError("cancel response lost"); throw new ApiError("forbidden", 403);
  });
  f.control.setTarget(target); await f.control.stop(); await f.control.stop();
  assert.equal(f.control.getSnapshot().phase, "rejected"); assert.equal(f.control.getSnapshot().stopRequested, true);
  f.control.begin(); assert.equal(f.control.getSnapshot().phase, "preparing");
  const restored = new SubmissionControl({ operations: { pending: () => null, recover: async () => null },
    scope: { ...scope, controlId: JSON.stringify([target.sessionId, target.agentRunId]) }, storage: f.store,
    cancel: async value => { assert.deepEqual(value, target); return terminal; } });
  assert.equal(restored.getSnapshot().stopRequested, true); assert.deepEqual(restored.getSnapshot().target, target);
  await restored.stop(); assert.equal(restored.getSnapshot().phase, "terminal"); assert.equal(calls, 2);
});

test("Stop settles a blocked file preparation immediately and its late completion cannot affect a new attempt", async () => {
  const bytes = deferred<ArrayBuffer>(); let reading = false; const posts = [];
  const f = setup(async (_path, options) => { const body = JSON.parse(options.body); posts.push(body); return accepted(body.operationId); });
  const file = new File(["synthetic attachment"], "synthetic.txt");
  Object.defineProperty(file, "arrayBuffer", { value: () => { reading = true; return bytes.promise; } });
  f.control.begin(); const sending = f.control.submit(hooks => f.operations.submit("/messages", input, [file], [], false, hooks));
  const rejected = assert.rejects(sending, /operation_dispatch_stopped/);
  await until(() => reading); await f.control.stop();
  try {
    assert.equal(f.control.getSnapshot().phase, "idle"); assert.equal(f.control.getSnapshot().feedback, "stoppedBeforeDispatch");
    await Promise.race([rejected, new Promise((_, reject) => setImmediate(() => reject(Error("local stop is still blocked on file preparation"))))]);
    f.control.begin(); await submit(f); const next = f.control.getSnapshot();
    f.operations.complete(f.operations.pending()!.operationId);
    bytes.resolve(new TextEncoder().encode("synthetic attachment").buffer);
    await new Promise(resolve => setTimeout(resolve, 20));
    assert.equal(posts.length, 1); assert.equal(f.control.getSnapshot(), next);
    assert.equal(f.operations.pending(), null);
  } finally { bytes.resolve(new ArrayBuffer(0)); await sending.catch(() => {}); }
});

test("retaining an older unknown stop never consumes malformed metadata belonging to its Run actor", async () => {
  let calls = 0; const f = setup(async () => null, async () => { if (++calls === 1) throw new TypeError("lost"); throw new ApiError("forbidden", 403); });
  f.control.setTarget(target); await f.control.stop(); await f.control.stop();
  const targetKey = `centaeris.submissionControl.v1:${JSON.stringify([scope.userId, scope.workspaceId, JSON.stringify([target.sessionId, target.agentRunId])])}`;
  f.store.setItem(targetKey, JSON.stringify({ schema: "web.submission_control.v1", operationId: null, target, stopRequested: true, terminal: null, unexpected: true }));
  assert.throws(() => f.control.begin(), /submission_control_storage_invalid/);
  assert.equal(f.control.getSnapshot().phase, "rejected"); assert.equal(f.control.getSnapshot().stopRequested, true);
});

test("a malformed cancellation acknowledgement remains unknown because acceptance cannot be disproved", async () => {
  const f = setup(async () => null, async () => ({ ...requested, agentRunId: "wrong-run" }));
  f.control.setTarget(target); await f.control.stop();
  assert.equal(f.control.getSnapshot().phase, "unknown"); assert.equal(f.control.getSnapshot().stopRequested, true);
  assert.equal(f.control.getSnapshot().error.message, "agent_run_cancel_response_invalid");
  assert.equal(f.control.getSnapshot().terminal, null);
});

test("a submission authorization rejection without a Stop intent reports submission rejection", async () => {
  const f = setup(async () => { throw new ApiError("forbidden", 403); });
  f.control.begin(); await assert.rejects(submit(f), /forbidden/);
  assert.equal(f.control.getSnapshot().phase, "rejected");
  assert.equal(f.control.getSnapshot().feedback, "submissionRejected");
  assert.equal(f.control.getSnapshot().stopRequested, false);
  assert.equal(f.control.getSnapshot().terminal, null);
});

test("receipt recovery rejection without a Stop intent reports submission rejection", async () => {
  let denied = false;
  const f = setup(async (_path, options) => {
    if (options) throw new TypeError("lost submission response");
    if (denied) throw new ApiError("forbidden", 403);
    throw new ApiError("operation_not_found", 404);
  });
  f.control.begin(); await assert.rejects(submit(f), /lost submission response/);
  const operationId = f.operations.pending()!.operationId;
  denied = true; await f.control.recover();
  assert.equal(f.control.getSnapshot().phase, "rejected");
  assert.equal(f.control.getSnapshot().feedback, "submissionRejected");
  assert.equal(f.control.getSnapshot().stopRequested, false);
  assert.equal(f.operations.pending()!.operationId, operationId);
});
