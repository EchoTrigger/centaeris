import assert from "node:assert/strict";
import test from "node:test";
import { TranscriptContentReader } from "../../src/chat/transcriptContentReader.ts";
import { ApiError } from "../../src/api.ts";
import { createWorkspaceTranscriptTransport } from "../../src/chat/transcriptTransport.ts";
import { watchTranscriptLoads } from "../helpers/transcript-reader-watchdog.mjs";

function source(text) {
  const bytes = new TextEncoder().encode(text);
  return async (offset) => {
    const start = Number(offset);
    let end = Math.min(start + 65536, bytes.length);
    while (end < bytes.length && (bytes[end] & 0xc0) === 0x80) end--;
    return { content: new TextDecoder("utf-8", { fatal: true }).decode(bytes.slice(start, end)),
      startOffset: offset, endOffset: String(end), hasMore: end < bytes.length };
  };
}

test("long Markdown loads automatically as one intact document across UTF-8 and fence boundaries", async () => {
  const text = `# Answer\n\n\`\`\`text\n${"中文abc".repeat(20000)}\n\`\`\`\n\n**Done**`;
  const reader = new TranscriptContentReader(source(text));
  await reader.loadAll();
  assert.equal(reader.getSnapshot().content, text);
  assert.equal(reader.getSnapshot().hasMore, false);
});

test("tool output appends ranges on demand and keeps earlier content readable", async () => {
  const totalBytes = 20 * 65536;
  const reader = new TranscriptContentReader(source("a".repeat(totalBytes)));
  await reader.loadMore();
  assert.equal(reader.getSnapshot().content.length, 65536);
  assert.equal(reader.getSnapshot().hasMore, true);
  while (reader.getSnapshot().hasMore) await reader.loadMore();
  assert.equal(reader.getSnapshot().content.length, totalBytes);
  assert.equal(reader.getSnapshot().hasMore, false);
});

test("completion subscribers can start the next page without losing its in-flight deduplication", async t => {
  const requests = [];
  const reader = new TranscriptContentReader(offset => new Promise(resolve => { requests.push({ offset, resolve }); }));
  t.after(() => reader.dispose());
  let continuation;
  reader.subscribe(() => {
    const state = reader.getSnapshot();
    if (state.content === "abc" && !state.loading) continuation = reader.loadMore();
  });
  const first = reader.loadMore();
  requests[0].resolve({ content: "abc", startOffset: "0", endOffset: "3", hasMore: true });
  await first;
  assert.deepEqual(requests.map(request => request.offset), ["0", "3"]);
  const duplicate = reader.loadMore();
  assert.equal(duplicate, continuation, "the old page's finally must not clear the new pending request");
  assert.equal(requests.length, 2);
  requests[1].resolve({ content: "def", startOffset: "3", endOffset: "6", hasMore: false });
  await duplicate;
  assert.equal(reader.getSnapshot().content, "abcdef");
});

test("disposing a view rejects late content even when the transport ignores cancellation", async () => {
  let resolve;
  const reader = new TranscriptContentReader(() => new Promise((done) => { resolve = done; }));
  const pending = reader.loadMore();
  reader.dispose();
  resolve({ content: "old session", startOffset: "0", endOffset: "11", hasMore: false });
  await pending;
  assert.equal(reader.getSnapshot().content, "");
});

for (const outcome of ["late page", "abort rejection"]) {
  test(`disposing during loadAll terminates after ${outcome}`, async () => {
    const watcher = watchTranscriptLoads(TranscriptContentReader);
    let resolve;
    let reject;
    let signal;
    let reads = 0;
    const reader = new TranscriptContentReader((_offset, requestSignal) => {
      reads++;
      signal = requestSignal;
      return new Promise((done, fail) => { resolve = done; reject = fail; });
    });
    let notifications = 0;
    reader.subscribe(() => { notifications++; });
    try {
      const pending = reader.loadAll();
      const before = reader.getSnapshot();
      reader.dispose();
      if (outcome === "late page") {
        resolve({ content: "old view", startOffset: "0", endOffset: "8", hasMore: true });
      } else {
        reject(new DOMException("Aborted", "AbortError"));
      }
      await pending;
      assert.equal(signal.aborted, true);
      assert.equal(reads, 1);
      assert.equal(reader.getSnapshot(), before);
      assert.equal(notifications, 1);
      await reader.loadAll();
      assert.equal(reads, 1);
    } finally {
      reader.dispose();
      watcher.restore();
    }
  });
}

test("unavailable historical output reports an error without automatic retry or fallback", async () => {
  const paths = [];
  const transport = createWorkspaceTranscriptTransport({
    request: async (path) => {
      paths.push(path);
      throw new ApiError("transcript_content_unavailable", 409);
    },
  });
  const identity = {
    sessionId: "session_1", projectionGeneration: "generation-1",
    reference: { refId: "tool-output:call-history", revision: "2", byteLength: "70000" },
  };
  const reader = new TranscriptContentReader((offset, signal) =>
    transport.loadContentRange(identity, offset, signal));
  await reader.loadAll();
  assert.equal(reader.getSnapshot().content, "");
  assert.equal(reader.getSnapshot().loading, false);
  assert.equal(reader.getSnapshot().error, true);
  assert.equal(paths.length, 1);
  await reader.loadAll();
  assert.equal(paths.length, 1);
  await reader.loadMore();
  assert.equal(paths.length, 1);
  assert.equal(reader.getSnapshot().error, true);
  reader.dispose();
});
