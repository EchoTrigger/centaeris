import assert from "node:assert/strict";
import { test } from "vitest";
import * as cache from "../src/lib/sessionViewCache.ts";

test("stores and validates session view cache projections", () => {
  const patch = cache.deriveReplayCursorPatch([
    { type: "session_event", agentRunId: "agent-run-a", cursor: 3 },
    {
      type: "session_event",
      agentRunId: "agent-run-a",
      cursor: 4,
      event: {
        taskId: "agent-run-a",
      },
    },
    {
      type: "session_event",
      agentRunId: "agent-run-b",
      cursor: 1,
      event: {
        taskId: "agent-run-b",
      },
    },
  ]);
  assert.deepEqual(patch, {
    "agent-run-a": 5,
    "agent-run-b": 2,
  });

  const merged = cache.mergeReplayCursors(
    {
      "agent-run-a": 8,
      "agent-run-c": 1,
    },
    patch,
  );
  assert.deepEqual(merged, {
    "agent-run-a": 8,
    "agent-run-b": 2,
    "agent-run-c": 1,
  });

  assert.throws(
    () => cache.mergeReplayCursors({}, { "agent-run-bad": -1 }),
    /invalid/,
  );

  const store = cache.createSessionViewCacheStore({ maxEntries: 2 });
  store.write({
    sessionId: "chat-a",
    snapshot: { messages: ["a"] },
    replayCursorsByAgentRunId: { "agent-run-a": 5 },
    verifiedReplayAgentRunIds: ["agent-run-a"],
  });
  store.write({
    sessionId: "chat-b",
    snapshot: { messages: ["b"] },
  });
  assert.equal(store.size(), 2);
  assert.deepEqual(store.get("chat-a").snapshot, { messages: ["a"] });

  store.write({
    sessionId: "chat-c",
    snapshot: { messages: ["c"] },
  });
  assert.equal(store.size(), 2);
  assert.equal(store.get("chat-b"), null);
  assert.deepEqual(store.get("chat-a").replayCursorsByAgentRunId, {
    "agent-run-a": 5,
  });
  assert.deepEqual(store.get("chat-a").verifiedReplayAgentRunIds, ["agent-run-a"]);
  assert.deepEqual(store.get("chat-c").snapshot, { messages: ["c"] });

  store.patchReplayCursors("chat-c", { "agent-run-c": 7 });
  assert.deepEqual(store.get("chat-c").replayCursorsByAgentRunId, {
    "agent-run-c": 7,
  });

  assert.deepEqual(
    cache.decideSessionViewCacheReplay({
      durableMessageIds: ["msg:user:turn-1", "msg:assistant:turn-1"],
      durableStreamAgentRunIds: ["agent-run-1"],
      cachedMessageIds: ["msg:user:turn-1", "msg:assistant:turn-1"],
      cachedReplayCursorsByAgentRunId: { "agent-run-1": 12 },
      cachedVerifiedReplayAgentRunIds: ["agent-run-1"],
    }),
    { kind: "incremental" },
  );

  assert.deepEqual(
    cache.decideSessionViewCacheReplay({
      durableMessageIds: ["msg:user:turn-1", "msg:assistant:turn-1"],
      durableStreamAgentRunIds: ["agent-run-1"],
      cachedMessageIds: ["msg:user:turn-1"],
      cachedReplayCursorsByAgentRunId: { "agent-run-1": 12 },
      cachedVerifiedReplayAgentRunIds: ["agent-run-1"],
    }),
    {
      kind: "fullReplay",
      reason: "missing_durable_message:msg:assistant:turn-1",
    },
  );

  assert.deepEqual(
    cache.decideSessionViewCacheReplay({
      durableMessageIds: ["msg:user:turn-1", "msg:assistant:turn-1"],
      durableStreamAgentRunIds: ["agent-run-1"],
      cachedMessageIds: ["msg:user:turn-1", "msg:assistant:turn-1"],
      cachedReplayCursorsByAgentRunId: {},
      cachedVerifiedReplayAgentRunIds: [],
    }),
    {
      kind: "fullReplay",
      reason: "missing_replay_cursor:agent-run-1",
    },
  );

  assert.deepEqual(
    cache.decideSessionViewCacheReplay({
      durableMessageIds: ["msg:user:turn-1", "msg:assistant:turn-1"],
      durableStreamAgentRunIds: ["agent-run-1"],
      cachedMessageIds: ["msg:user:turn-1", "msg:assistant:turn-1"],
      cachedReplayCursorsByAgentRunId: { "agent-run-1": 12 },
      cachedVerifiedReplayAgentRunIds: [],
    }),
    {
      kind: "fullReplay",
      reason: "unverified_replay_projection:agent-run-1",
    },
  );

  assert.deepEqual(
    cache.decideSessionViewCacheReplay({
      durableMessageIds: ["msg:user:turn-2", "msg:assistant:turn-2"],
      durableStreamAgentRunIds: ["task-2"],
      cachedMessageIds: ["msg:user:turn-2", "msg:assistant:turn-2"],
      cachedReplayCursorsByAgentRunId: { "task-rolled-back": 1, "task-2": 7 },
      cachedVerifiedReplayAgentRunIds: ["task-rolled-back", "task-2"],
    }),
    {
      kind: "fullReplay",
      reason: "stale_replay_cursor:task-rolled-back",
    },
  );

  store.clear();
  assert.equal(store.size(), 0);
});
