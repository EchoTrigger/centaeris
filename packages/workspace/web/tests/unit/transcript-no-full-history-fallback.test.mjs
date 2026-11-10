import assert from "node:assert/strict";
import test from "node:test";
import { createWorkspaceTranscriptTransport } from "../../src/chat/transcriptTransport.ts";

test("failed tail and older reads surface the page error without additional requests", async () => {
  const paths = [];
  const failure = new TypeError("page unavailable");
  const transport = createWorkspaceTranscriptTransport({ request: async (path) => {
    paths.push(path);
    throw failure;
  } });
  const signal = new AbortController().signal;
  await assert.rejects(transport.loadTail("s", signal), error => error === failure);
  assert.deepEqual(paths, ["/api/sessions/s/transcript"]);
  await assert.rejects(transport.loadOlder({ sessionId: "s", projectionVersion: "transcript.projection.v1",
    projectionGeneration: "g", sourceHighWater: "12" }, "before-1", signal), error => error === failure);
  assert.deepEqual(paths, ["/api/sessions/s/transcript",
    "/api/sessions/s/transcript?sourceHighWater=12&projectionGeneration=g&olderCursor=before-1"]);
});
