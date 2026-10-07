import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";

import {
  createTranscriptViewStore,
  validateTranscriptPage,
  validateTranscriptPatchPage,
} from "../../src/chat/transcriptViewStore.ts";
import { ApiError } from "../../src/api.ts";
import { createWorkspaceTranscriptTransport } from "../../src/chat/transcriptTransport.ts";
import { WorkspaceTranscriptController } from "../../src/chat/workspaceTranscriptController.ts";
import { streamWorkspaceAgentRun } from "../../src/chat/workspaceWebTransport.ts";
import { readSse } from "../../src/chat/sessionStreamProtocol.ts";

const sessionId = "session_1";
const projectionVersion = "transcript.projection.v1";
const projectionGeneration = "generation-1";

test("Web accepts current Core-generated blocks including optional presentation facts", () => {
  const samples = JSON.parse(readFileSync(new URL(
    "../../../api/app_core/generated/transcript_block.samples.json", import.meta.url), "utf8"));
  assert.ok(samples.some((sample) => sample.presentation !== null && sample.presentation !== undefined));
  for (const sample of samples) {
    const result = validateTranscriptPage(page({ blocks: [sample] }), { sessionId });
    assert.deepEqual(result.blocks[0], sample);
  }
});

test("presentation facts retain strict keys and reject invalid durations", () => {
  const sample = block("tool:presented", 1, 1, "tool", "result");
  const facts = { agentRunId: "run:1", sourceType: "tool_result", observedAtMs: 1000,
    displayTarget: "result.txt", durationMs: 12, operation: { kind: "read", status: "ok" } };
  assert.doesNotThrow(() => validateTranscriptPage(page({ blocks: [{ ...sample, presentation: null }] }), { sessionId }));
  assert.doesNotThrow(() => validateTranscriptPage(page({ blocks: [{ ...sample, presentation: facts }] }), { sessionId }));
  for (const presentation of [{ ...facts, extra: true }, { ...facts, durationMs: -1 },
    { ...facts, observedAtMs: "1000" }, { ...facts, displayTarget: 42 }]) {
    assert.throws(() => validateTranscriptPage(page({ blocks: [{ ...sample, presentation }] }), { sessionId }));
  }
  assert.throws(() => validateTranscriptPage(page({ blocks: [{ ...sample, presentation: facts, extra: true }] }), { sessionId }));
});

function terminalResponse() {
  return new Response(`id: cursor:11\ndata: ${JSON.stringify({
    schema: "session.stream.item.v1", kind: "committed", agentRunId: "run:1", sourceSequence: 11,
    event: { schemaVersion: "session.event.v1", eventVersion: 1, eventId: "event:11",
      sessionId, agentRunId: "run:1", turnId: "turn:1", sequence: 11, createdAtMs: 1,
      type: "agent_run_completed", payload: {} },
  })}\n\n`);
}

test("a failed patch preserves visible content and can resume with a fresh controller", async () => {
  const store = createTranscriptViewStore();
  const epoch = store.openTail(page({ blocks: [block("tail", 1, 10)] }));
  const problem = new Error("conflicting transcript block revision");
  let patches = 0;
  let recoveries = 0;
  let requests = 0;
  let failedStreamCancelled = false;
  const createController = () => new WorkspaceTranscriptController({
    store, viewEpoch: epoch, identity: { sessionId, projectionVersion, projectionGeneration },
    agentRunId: "run:1", initialCursor: "cursor:10",
    scheduleFrame: (callback) => (queueMicrotask(callback), 1), cancelFrame: () => {},
    transport: { loadPatches: async (_identity, _signal, apply) => {
      if (++patches === 1) throw problem;
      apply(patchPage({ patches: [{ sourceHighWater: "11", upserts: [block("next", 1, 11)] }], through: "11" }));
      return "11";
    } },
  });
  const controller = createController();
  await streamWorkspaceAgentRun({
    controller, signal: new AbortController().signal, onConnection() {},
    request: async () => {
      if (++requests > 1) return terminalResponse();
      const data = new TextEncoder().encode(await terminalResponse().text());
      return new Response(new ReadableStream({
        start(reader) { reader.enqueue(data); },
        cancel() { failedStreamCancelled = true; },
      }));
    }, wait: async () => {},
    recover: async (error) => {
      assert.equal(error, problem);
      assert.equal(store.getBlockSnapshot("tail").body.content.inlineContent, "tail");
      assert.equal(controller.lastCursor, "cursor:10");
      recoveries++;
      controller.dispose();
      return createController();
    },
  });
  assert.equal(recoveries, 1);
  assert.equal(failedStreamCancelled, true);
  assert.equal(store.getBlockSnapshot("next").body.content.inlineContent, "next");
});

test("SSE cancellation releases a pending reader without waiting for another token", async () => {
  let cancelled = false;
  const response = new Response(new ReadableStream({ cancel() { cancelled = true; } }));
  const abort = new AbortController();
  const reading = readSse(response, "run:1", () => {}, { signal: abort.signal });
  abort.abort();
  await assert.rejects(Promise.race([reading, new Promise((_, reject) => {
    setTimeout(() => reject(new Error("reader remained blocked")), 100);
  })]), { name: "AbortError" });
  assert.equal(cancelled, true);
});

test("transient HTTP failures retry with bounded backoff and authorization failures do not", async () => {
  const controller = { sessionId, agentRunId: "run:1", lastCursor: "cursor:10",
    acceptWithBackpressure: async () => {}, whenIdle: async () => {}, setCursor() {} };
  const delays = [];
  let requests = 0;
  await streamWorkspaceAgentRun({ controller, signal: new AbortController().signal,
    onConnection() {},
    request: async () => {
      if (++requests <= 2) throw new ApiError("temporarily unavailable", requests === 1 ? 503 : 429);
      return terminalResponse();
    }, wait: async (ms) => { delays.push(ms); },
  });
  assert.deepEqual(delays, [500, 1000]);
  requests = 0;
  await assert.rejects(streamWorkspaceAgentRun({ controller, signal: new AbortController().signal,
    onConnection() {},
    request: async () => { requests++; throw new ApiError("forbidden", 403); },
    wait: async () => { throw new Error("must not retry"); },
  }), /forbidden/);
  assert.equal(requests, 1);
});

test("failed recovery is bounded and reports its actual error", async () => {
  const failureAbort = new AbortController();
  failureAbort.abort(new Error("original projection failure"));
  const controller = { sessionId, agentRunId: "run:1", lastCursor: "cursor:10",
    failureSignal: failureAbort.signal,
    acceptWithBackpressure: async () => {}, whenIdle: async () => {}, setCursor() {} };
  const unavailable = new ApiError("snapshot unavailable", 503);
  const delays = [];
  let recoveries = 0;
  await assert.rejects(streamWorkspaceAgentRun({ controller, signal: new AbortController().signal,
    onConnection() {},
    request: async () => { throw new DOMException("Aborted", "AbortError"); },
    recover: async () => { recoveries++; throw unavailable; },
    wait: async (delay) => { delays.push(delay); },
  }), (error) => error === unavailable);
  assert.equal(recoveries, 5);
  assert.deepEqual(delays, [500, 1000, 2000, 4000, 8000]);
});

function content(text) {
  return { inlineContent: text, sourceRef: null };
}

function block(blockId, revision, sequence, kind = "assistantText", text = blockId) {
  const body = kind === "userText"
    ? { kind, content: content(text) }
    : kind === "tool"
      ? {
          kind,
          callId: blockId.replace(/^tool:/, ""),
          toolName: "read",
          status: "completed",
          summary: text,
          summaryRef: null,
          outputRef: null,
        }
      : { kind, content: content(text), status: "completed" };
  return {
    blockId,
    blockRevision: String(revision),
    orderKey: { sourceSequence: String(sequence), ordinal: 0 },
    body,
  };
}

function page({ blocks, sourceHighWater = "10", olderCursor = null }) {
  return {
    schema: "transcript.page.v1",
    sessionId,
    projectionVersion,
    projectionGeneration,
    sourceHighWater,
    blocks,
    olderCursor,
    hasOlder: olderCursor !== null,
    resumeCursors: sourceHighWater === "0"
      ? []
      : [{ streamId: "workspace-transcript.v1", cursor: sourceHighWater }],
  };
}

function patchPage({ patches, after = "10", through = "12", next = through, hasMore = false }) {
  return {
    schema: "transcript.patch.page.v1",
    sessionId,
    projectionVersion,
    projectionGeneration,
    throughSourceHighWater: through,
    patches: patches.map((item) => ({
      schema: "transcript.patch.v1",
      sessionId,
      projectionVersion,
      projectionGeneration,
      sourceHighWater: item.sourceHighWater,
      streamId: "workspace-transcript.v1",
      appliedCursor: item.sourceHighWater,
      upserts: item.upserts || [],
      removals: item.removals || [],
    })),
    nextSourceHighWater: next,
    hasMore,
  };
}

test("transcript page and patch DTOs loud-fail on nested drift", () => {
  const accepted = validateTranscriptPage(page({ blocks: [block("assistant:1", 1, 10)] }), { sessionId });
  assert.equal(accepted.blocks[0].body.content.inlineContent, "assistant:1");

  const unknownNested = page({ blocks: [block("assistant:1", 1, 10)] });
  unknownNested.blocks[0].body.content.legacyText = "banana";
  assert.throws(() => validateTranscriptPage(unknownNested, { sessionId }), /invalid transcript page/);

  const wrongGeneration = patchPage({ patches: [] });
  wrongGeneration.projectionGeneration = "generation-2";
  assert.throws(() => validateTranscriptPatchPage(wrongGeneration, {
    sessionId,
    projectionVersion,
    projectionGeneration,
    afterSourceHighWater: "10",
  }), /invalid transcript patch page/);
});

test("late older pages cannot overwrite a newer committed block revision", () => {
  const store = createTranscriptViewStore();
  const epoch = store.openTail(page({
    blocks: [block("assistant:tail", 1, 10)],
    olderCursor: "older-1",
  }));

  store.applyPatchPage(patchPage({
    patches: [{ sourceHighWater: "11", upserts: [block("tool:1", 2, 2, "tool")] }],
    through: "11",
  }), epoch);

  store.prependPage(page({
    blocks: [block("user:1", 1, 1, "userText"), block("tool:1", 1, 2, "tool")],
    olderCursor: null,
  }), epoch);

  assert.deepEqual(store.getListSnapshot().blockIds, ["user:1", "tool:1", "assistant:tail"]);
  assert.equal(store.getBlockSnapshot("tool:1").blockRevision, "2");
  assert.equal(store.getListSnapshot().appliedSourceHighWater, "11");
});

test("patch application is atomic and advances only after a valid merge", () => {
  const store = createTranscriptViewStore();
  const epoch = store.openTail(page({ blocks: [block("assistant:tail", 1, 10)] }));
  const invalid = patchPage({
    patches: [{ sourceHighWater: "11", upserts: [block("assistant:tail", 1, 9)] }],
    through: "11",
  });
  assert.throws(() => store.applyPatchPage(invalid, epoch), /conflicting transcript block revision/);
  assert.equal(store.getListSnapshot().appliedSourceHighWater, "10");
  assert.equal(store.getBlockSnapshot("assistant:tail").orderKey.sourceSequence, "10");
});

test("an existing block revision updates only that block and cannot move its order key", () => {
  const store = createTranscriptViewStore();
  const epoch = store.openTail(page({
    blocks: [block("tool:1", 1, 9, "tool"), block("assistant:tail", 1, 10)],
  }));
  let listNotifications = 0;
  let blockNotifications = 0;
  const unsubscribeList = store.subscribeList(() => { listNotifications += 1; });
  const unsubscribeBlock = store.subscribeBlock("tool:1", () => { blockNotifications += 1; });

  store.applyPatchPage(patchPage({
    patches: [{ sourceHighWater: "11", upserts: [block("tool:1", 2, 9, "tool", "done")] }],
    through: "11",
  }), epoch);
  assert.equal(listNotifications, 0);
  assert.equal(blockNotifications, 1);
  assert.deepEqual(store.getListSnapshot().blockIds, ["tool:1", "assistant:tail"]);

  assert.throws(() => store.applyPatchPage(patchPage({
    after: "11",
    patches: [{ sourceHighWater: "12", upserts: [block("tool:1", 3, 8, "tool", "moved")] }],
    through: "12",
  }), epoch), /orderKey/);
  assert.equal(store.getListSnapshot().appliedSourceHighWater, "11");
  assert.equal(store.getBlockSnapshot("tool:1").body.summary, "done");
  unsubscribeList();
  unsubscribeBlock();
});

test("stale epochs cannot mutate a newly opened Session view", () => {
  const store = createTranscriptViewStore();
  const staleEpoch = store.openTail(page({ blocks: [block("old", 1, 10)] }));
  const currentEpoch = store.openTail(page({ blocks: [block("current", 1, 10)] }));
  assert.notEqual(staleEpoch, currentEpoch);
  assert.equal(store.prependPage(page({ blocks: [block("stale", 1, 1)] }), staleEpoch), false);
  assert.deepEqual(store.getListSnapshot().blockIds, ["current"]);
});

test("releasing loaded history keeps the tail and committed additions reloadable", () => {
  const store = createTranscriptViewStore();
  const epoch = store.openTail(page({
    blocks: [block("assistant:tail", 1, 10)],
    olderCursor: "older-1",
  }));
  store.prependPage(page({
    blocks: [
      block("user:old", 1, 1, "userText"),
      block("assistant:old-unmodified", 1, 2),
    ],
    olderCursor: null,
  }), epoch);
  store.applyPatchPage(patchPage({
    patches: [{
      sourceHighWater: "11",
      upserts: [
        block("user:old", 2, 1, "userText"),
        block("assistant:new", 1, 11),
      ],
    }],
    through: "11",
  }), epoch);
  const bytesBeforeRelease = store.managedContentBytes();

  store.releaseLoadedHistory();

  assert.deepEqual(
    store.getListSnapshot().blockIds,
    ["user:old", "assistant:tail", "assistant:new"],
  );
  assert.equal(store.getListSnapshot().olderCursor, "older-1");
  assert.equal(store.getListSnapshot().hasOlder, true);
  assert.ok(store.managedContentBytes() > 0);
  assert.ok(store.managedContentBytes() < bytesBeforeRelease);
});

test("tool content transport binds the range response to its reference", async () => {
  const paths = [];
  const transport = createWorkspaceTranscriptTransport({
    request: async (path) => {
      paths.push(path);
      return new Response(JSON.stringify({
        schema: "transcript.content.range.v1",
        sessionId,
        projectionVersion,
        projectionGeneration,
        refId: "tool-output:call-1",
        revision: "2",
        byteLength: "70000",
        startOffset: "0",
        endOffset: "3",
        content: "世",
        hasMore: true,
      }));
    },
  });
  const result = await transport.loadContentRange({
    sessionId,
    projectionGeneration,
    reference: { refId: "tool-output:call-1", revision: "2", byteLength: "70000" },
  }, "0", new AbortController().signal);
  assert.equal(result.content, "世");
  assert.deepEqual(paths, [
    `/api/sessions/${sessionId}/transcript/content?projectionGeneration=generation-1&refId=tool-output%3Acall-1&revision=2&byteLength=70000&offset=0`,
  ]);
});

test("tail projection retries freeze the first reported waterline and generation", async () => {
  const paths = [];
  const waits = [];
  const transport = createWorkspaceTranscriptTransport({
    request: async (path) => {
      paths.push(path);
      if (paths.length === 1) {
        throw new ApiError("transcript_projection_not_ready", 409, {
          error: "transcript_projection_not_ready",
          projectionGeneration,
          sourceHighWater: "10",
          projectedHighWater: "2",
        });
      }
      return new Response(JSON.stringify(page({ blocks: [block("tail", 1, 10)] })));
    },
    wait: async (delayMs) => { waits.push(delayMs); },
  });

  const result = await transport.loadTail(sessionId, new AbortController().signal);
  assert.equal(result.sourceHighWater, "10");
  assert.deepEqual(paths, [
    `/api/sessions/${sessionId}/transcript`,
    `/api/sessions/${sessionId}/transcript?sourceHighWater=10&projectionGeneration=generation-1`,
  ]);
  assert.deepEqual(waits, [50]);
});

test("patch continuation keeps one through-waterline until fully applied", async () => {
  const paths = [];
  const applied = [];
  const first = patchPage({
    patches: [{ sourceHighWater: "11", upserts: [block("next", 1, 11)] }],
    through: "12",
    next: "11",
    hasMore: true,
  });
  const second = patchPage({
    patches: [{ sourceHighWater: "12", upserts: [] }],
    after: "11",
    through: "12",
  });
  const responses = [first, second];
  const transport = createWorkspaceTranscriptTransport({
    request: async (path) => {
      paths.push(path);
      if (paths.length === 2) assert.equal(applied.length, 1);
      return new Response(JSON.stringify(responses.shift()));
    },
  });
  const appliedHighWater = await transport.loadPatches({
    sessionId,
    projectionVersion,
    projectionGeneration,
    afterSourceHighWater: "10",
  }, new AbortController().signal, (item) => applied.push(item));
  assert.equal(appliedHighWater, "12");
  assert.equal(applied.length, 2);
  assert.deepEqual(paths, [
    `/api/sessions/${sessionId}/transcript/patches?afterSourceHighWater=10&projectionGeneration=generation-1`,
    `/api/sessions/${sessionId}/transcript/patches?afterSourceHighWater=11&throughSourceHighWater=12&projectionGeneration=generation-1`,
  ]);
});

test("live overlay is isolated from unrelated patches and sealed by its committed block", () => {
  const store = createTranscriptViewStore();
  const epoch = store.openTail(page({ blocks: [block("user:1", 1, 10, "userText")] }));
  assert.equal(store.applyLiveOverlay({
    messageId: "assistant:live",
    turnId: "turn:1",
    afterSourceHighWater: "10",
    revision: 1,
    text: "draft",
    reasoning: null,
  }, epoch), true);
  assert.equal(store.getLiveSnapshot().text, "draft");

  store.applyPatchPage(patchPage({
    patches: [{ sourceHighWater: "11", upserts: [block("tool:1", 1, 11, "tool")] }],
    through: "11",
  }), epoch);
  assert.equal(store.getLiveSnapshot().text, "draft");

  store.applyPatchPage(patchPage({
    after: "11",
    patches: [{ sourceHighWater: "12", upserts: [block("assistant:live", 1, 12)] }],
    through: "12",
  }), epoch);
  assert.equal(store.getLiveSnapshot(), null);
});

test("active AgentRun discovery is bound to the page waterline", async () => {
  const paths = [];
  const transport = createWorkspaceTranscriptTransport({
    request: async (path) => {
      paths.push(path);
      return new Response(JSON.stringify({
        schema: "workspace.transcript.active_agent_run.v1",
        sessionId,
        agentRun: {
          agentRunId: "run:1",
          status: "running",
          streamCursor: "cursor:10",
        },
      }));
    },
  });
  const active = await transport.loadActiveAgentRun({
    sessionId,
    projectionVersion,
    projectionGeneration,
    sourceHighWater: "10",
  }, new AbortController().signal);
  assert.equal(active.agentRun.agentRunId, "run:1");
  assert.deepEqual(paths, [
    `/api/sessions/${sessionId}/transcript/active-agent-run?sourceHighWater=10`,
  ]);
});

test("optional tool detail failures do not block committed text or advance the cursor before patches", async () => {
  const store = createTranscriptViewStore();
  const epoch = store.openTail(page({ blocks: [block("tail", 1, 10)] }));
  const frames = [];
  const detailErrors = [];
  const controller = new WorkspaceTranscriptController({
    store,
    viewEpoch: epoch,
    identity: {
      sessionId,
      projectionVersion,
      projectionGeneration,
    },
    agentRunId: "run:1",
    initialCursor: "cursor:10",
    onCommittedEvent: () => { throw new Error("tool operation call binding mismatch"); },
    onDetailError: (error) => detailErrors.push(error.message),
    transport: {
      loadPatches: async (_identity, _signal, apply) => {
        apply(patchPage({
          patches: [{ sourceHighWater: "11", upserts: [block("next", 1, 11)] }],
          through: "11",
        }));
        return "11";
      },
    },
    scheduleFrame: (callback) => (frames.push(callback), frames.length),
    cancelFrame: () => {},
  });
  controller.accept({
    cursor: "cursor:11",
    item: {
      schema: "session.stream.item.v1",
      kind: "committed",
      agentRunId: "run:1",
      sourceSequence: 11,
      event: { type: "tool_result" },
    },
  });
  assert.equal(controller.lastCursor, "cursor:10");
  await frames.shift()();
  await controller.whenIdle();
  assert.equal(controller.lastCursor, "cursor:11");
  assert.equal(store.getBlockSnapshot("next").blockRevision, "1");
  assert.deepEqual(detailErrors, ["tool operation call binding mismatch"]);
});


test("admission failure restores control to the caller without recovery swallowing it", async () => {
  const controller = { sessionId, agentRunId: "run:1", lastCursor: "0-0",
    acceptWithBackpressure: async () => {}, whenIdle: async () => {}, setCursor() {} };
  await assert.rejects(streamWorkspaceAgentRun({ controller, signal: new AbortController().signal,
    onConnection() {}, request: async () => { throw new ApiError("agent_run_not_admitted", 409); },
    recover: async () => { throw new Error("must not recover an unadmitted run"); },
    wait: async () => { throw new Error("must not wait"); },
  }), /agent_run_not_admitted/);
});
