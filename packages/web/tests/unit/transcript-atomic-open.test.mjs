import assert from "node:assert/strict";
import test from "node:test";
import { createTranscriptViewStore } from "../../src/chat/transcriptViewStore.ts";

const block = (id, sequence, text = id) => ({ blockId: id, blockRevision: "1", orderKey: { sourceSequence: String(sequence), ordinal: 0 },
  body: { kind: "assistantText", status: "completed", content: { inlineContent: text, sourceRef: null } } });
const page = (blocks, olderCursor, highWater = "100", generation = "new") => ({ schema: "transcript.page.v1", sessionId: "session", projectionVersion: "transcript.projection.v1",
  projectionGeneration: generation, sourceHighWater: highWater, blocks, olderCursor, hasOlder: olderCursor !== null,
  resumeCursors: [{ streamId: "workspace-transcript.v1", cursor: highWater }] });

test("fresh tail and bounded older pages publish as one coherent view", () => {
  const store = createTranscriptViewStore();
  store.openTail(page([block("old", 70)], null, "70", "old"));
  const publications = [];
  store.subscribeList(() => publications.push(store.getListSnapshot().blockIds));
  const epoch = store.openTail(page([block("tail", 100)], "older-1"), [page([block("middle", 70)], "older-2"), page([block("reading", 30)], "older-3")]);
  assert.deepEqual(publications, [["reading", "middle", "tail"]]);
  assert.deepEqual(store.getListSnapshot().blockIds, ["reading", "middle", "tail"]);
  assert.equal(store.getListSnapshot().viewEpoch, epoch);
  assert.equal(store.getListSnapshot().olderCursor, "older-3");
  assert.equal(store.getBlockSnapshot("old"), null);
  store.releaseLoadedHistory();
  assert.deepEqual(store.getListSnapshot().blockIds, ["tail"], "releasing loaded history still keeps just the original tail");
  assert.equal(store.getListSnapshot().olderCursor, "older-1");
});

for (const [name, older] of [
  ["generation", page([block("reading", 30)], null, "100", "other")],
  ["waterline", page([block("reading", 30)], null, "99")],
  ["overlapping order", page([block("reading", 100)], null)],
  ["repeated cursor", page([block("reading", 30)], "older-1")],
  ["empty progress", page([], "older-2")],
  ["inline budget", page([block("reading", 30, "a".repeat(65537))], null)],
  ["unknown field", { ...page([block("reading", 30)], null), unexpected: true }],
]) test(`invalid replacement ${name} leaves the current view untouched`, () => {
  const store = createTranscriptViewStore();
  store.openTail(page([block("old", 70)], null, "70", "old"));
  const before = store.getListSnapshot();
  assert.throws(() => store.openTail(page([block("tail", 100)], "older-1"), [older]));
  assert.equal(store.getListSnapshot(), before);
  assert.equal(store.getBlockSnapshot("old").body.content.inlineContent, "old");
});
