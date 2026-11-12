import assert from "node:assert/strict";
import test from "node:test";
import { readChatMarkdown } from "../../src/chat/chatMarkdown.ts";

test("copies older messages and ranged Unicode content in order at the tail snapshot", async () => {
  const reference = { refId: "long", revision: "1", byteLength: "10" };
  const block = (sequence, kind, content) => ({ orderKey: { sourceSequence: sequence, ordinal: 0 }, body: { kind, content } });
  const identity = { sessionId: "s", projectionVersion: "v1", projectionGeneration: "g", sourceHighWater: "8" };
  const signal = new AbortController().signal;
  const transport = {
    async loadTail(id, receivedSignal) {
      assert.equal(id, "s"); assert.equal(receivedSignal, signal);
      return { ...identity, blocks: [block("8", "assistantText", { inlineContent: null, sourceRef: reference })], olderCursor: "older" };
    },
    async loadOlder(receivedIdentity, cursor) {
      assert.deepEqual(receivedIdentity, identity); assert.equal(cursor, "older");
      return { blocks: [block("1", "userText", { inlineContent: "**Question**", sourceRef: null }), block("2", "reasoning", { inlineContent: "internal", sourceRef: null })], olderCursor: null };
    },
    async loadContentRange(receivedIdentity, offset) {
      assert.deepEqual(receivedIdentity, { ...identity, reference });
      return offset === "0" ? { content: "中文", hasMore: true, endOffset: "6" } : { content: "🙂", hasMore: false, endOffset: "10" };
    },
  };
  assert.equal(await readChatMarkdown(transport, { id: "s", title: "Original title" }, signal), "# Original title\n\n## User\n\n**Question**\n\n## Agent\n\n中文🙂\n");
});

test("a failed older-page read rejects the export instead of returning a partial chat", async () => {
  const denied = new Error("permission denied");
  const transport = { loadTail: async () => ({ blocks: [], olderCursor: "older" }), loadOlder: async () => { throw denied; } };
  await assert.rejects(readChatMarkdown(transport, { id: "s", title: "Title" }, new AbortController().signal), denied);
});
