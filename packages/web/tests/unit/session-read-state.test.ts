import assert from "node:assert/strict";
import { test } from "node:test";
import { SessionReadController, mergeSessionAttention } from "../../src/shell/sessionReadState.ts";

const session = (changes = {}) => ({ id: "session", workspaceId: "workspace", agentId: "agent", isUnread: true, isManuallyUnread: false, latestAttentionSequence: "10", readThroughSequence: "0", attentionRevision: "1", ...changes });
const observation = (changes = {}) => ({ session: session(), visible: true, readingLatest: true, appliedSourceHighWater: "10", ...changes });
const flush = () => new Promise(resolve => setImmediate(resolve));

test("visible applied content is acknowledged once, independently of sidebar metadata mutations", async () => {
  const calls = []; const results = [];
  const controller = new SessionReadController({
    send: async (record, through, signal) => { calls.push([record.id, through, signal.aborted]); return session({ readThroughSequence: through, isUnread: false, attentionRevision: "2" }); },
    onAcknowledged: record => results.push(record),
  });
  controller.observe(observation()); controller.observe(observation());
  await flush(); controller.observe(observation()); await flush();
  assert.deepEqual(calls.map(([id, through]) => [id, through]), [["session", "10"]]);
  assert.equal(results[0].readThroughSequence, "10");
  controller.dispose();
});

test("background, manual-unread, old-reading and unapplied notifications stay unread", async () => {
  const controller = new SessionReadController({ send: async () => assert.fail("unseen content acknowledged"), onAcknowledged: () => {} });
  for (const value of [observation({ visible: false }), observation({ readingLatest: false }), observation({ appliedSourceHighWater: "9" }), observation({ session: session({ isManuallyUnread: true }) })]) controller.observe(value);
  await flush(); controller.dispose();
});

test("a newer notification arriving during an acknowledgement is not erased or dropped", async () => {
  let release; const calls = [];
  const controller = new SessionReadController({ send: async (_record, through) => {
    calls.push(through);
    if (through === "10") await new Promise(resolve => { release = resolve; });
    return session({ readThroughSequence: through, latestAttentionSequence: "11", attentionRevision: through === "10" ? "3" : "4", isUnread: through !== "11" });
  }, onAcknowledged: () => {} });
  controller.observe(observation()); await flush();
  controller.observe(observation({ session: session({ latestAttentionSequence: "11", attentionRevision: "2" }), appliedSourceHighWater: "11" }));
  assert.deepEqual(calls, ["10"]);
  release(); await flush(); await flush();
  assert.deepEqual(calls, ["10", "11"]); controller.dispose();
});

test("failed acknowledgement waits for a new observation instead of polling", async () => {
  let calls = 0;
  const controller = new SessionReadController({ send: async () => { calls++; throw new TypeError("offline"); }, onAcknowledged: () => {} });
  controller.observe(observation()); await flush(); await flush();
  assert.equal(calls, 1);
  controller.observe(observation()); await flush(); assert.equal(calls, 2); controller.dispose();
});

test("an old sidebar snapshot cannot resurrect read notifications or reverse a manual choice", () => {
  const read = session({ isUnread: false, readThroughSequence: "10", attentionRevision: "3" });
  assert.equal(mergeSessionAttention(read, session()).isUnread, false);
  assert.equal(mergeSessionAttention(read, session()).readThroughSequence, "10");
  const manual = session({ isManuallyUnread: true, readThroughSequence: "10", attentionRevision: "4" });
  assert.equal(mergeSessionAttention(manual, read).isManuallyUnread, true);
  assert.equal(mergeSessionAttention(manual, read).isUnread, true);
});

test("attention identities and monotonic read contracts reject contradictory metadata", () => {
  assert.throws(() => mergeSessionAttention(session(), session({ id: "foreign" })), /identity/);
  assert.throws(() => mergeSessionAttention(session({ readThroughSequence: "10", attentionRevision: "3", isUnread: false }), session({ readThroughSequence: "9", attentionRevision: "4" })), /regress/);
});

test("equal attention revisions cannot contradict manual or observed-read state", () => {
  const current = session();
  for (const incoming of [session({ isManuallyUnread: true }), session({ readThroughSequence: "1" }), session({ latestAttentionSequence: "11" })]) {
    assert.throws(() => mergeSessionAttention(current, incoming), /revision|conflict/);
  }
  assert.equal(mergeSessionAttention({ ...current, title: "Before" }, { ...current, title: "After" }).title, "After");
});

test("malformed attention values reject instead of silently normalizing a contradictory blue dot", () => {
  for (const incoming of [session({ isUnread: false }), session({ isUnread: "true" }), session({ id: 1 }),
    session({ latestAttentionSequence: "01" }), session({ readThroughSequence: "1\n" }),
    session({ attentionRevision: "18446744073709551616" })]) {
    assert.throws(() => mergeSessionAttention(session(), incoming));
  }
});

test("invalid observations report one error without launching a request or an unhandled rejection", async () => {
  const errors = []; let calls = 0;
  const controller = new SessionReadController({
    send: async (_record, through) => { calls++; return session({ readThroughSequence: through, isUnread: false, attentionRevision: "2" }); },
    onAcknowledged: () => {}, onError: error => errors.push(error),
  });
  controller.observe(observation({ session: session({ latestAttentionSequence: "01" }) }));
  await flush(); await flush();
  assert.equal(calls, 0); assert.equal(errors.length, 1);
  controller.observe(observation()); await flush();
  assert.equal(calls, 1); controller.dispose();
});

test("a rejected acknowledgement application can be retried by the next valid observation", async () => {
  let calls = 0, deliveries = 0; const errors = [];
  const controller = new SessionReadController({
    send: async (_record, through) => { calls++; return session({ readThroughSequence: through, isUnread: false, attentionRevision: "2" }); },
    onAcknowledged: () => { if (++deliveries === 1) throw new Error("view rejected acknowledgement"); },
    onError: error => errors.push(error),
  });
  controller.observe(observation()); await flush();
  assert.equal(errors.length, 1);
  controller.observe(observation()); await flush();
  assert.equal(calls, 2); assert.equal(deliveries, 2); controller.dispose();
});
