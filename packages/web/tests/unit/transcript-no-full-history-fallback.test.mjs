import assert from "node:assert/strict";
import test from "node:test";
import { createWorkspaceTranscriptTransport } from "../../src/chat/transcriptTransport.ts";
import { streamWorkspaceAgentRun } from "../../src/chat/workspaceWebTransport.ts";

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

test("reconnecting a failed stream repeats only the AgentRun events request at its committed cursor", async () => {
  const requests = [];
  const connections = [];
  const delays = [];
  const abort = new AbortController();
  await streamWorkspaceAgentRun({
    controller: { sessionId: "s", agentRunId: "run", lastCursor: "cursor:12",
      acceptWithBackpressure: async () => {}, whenIdle: async () => {}, setCursor() {} },
    signal: abort.signal,
    onConnection: value => connections.push(value),
    request: async (path, options) => {
      requests.push({ path, headers: options.headers });
      if (requests.length === 1) throw new TypeError("connection interrupted");
      return new Response(`id: cursor:13\ndata: ${JSON.stringify({
        schema: "session.stream.item.v1", kind: "committed", agentRunId: "run", sourceSequence: 13,
        event: { schemaVersion: "session.event.v1", eventVersion: 1, eventId: "event:13",
          sessionId: "s", agentRunId: "run", turnId: "turn", sequence: 13, createdAtMs: 1,
          type: "agent_run_completed", payload: {} },
      })}\n\n`);
    },
    wait: async delay => delays.push(delay),
  });
  assert.deepEqual(requests, Array(2).fill({
    path: "/api/sessions/s/agent-runs/run/events", headers: { "Last-Event-ID": "cursor:12" },
  }));
  assert.deepEqual(connections, ["reconnecting", "running"]);
  assert.deepEqual(delays, [500]);
});
