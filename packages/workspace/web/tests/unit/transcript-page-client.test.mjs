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

const sessionId = "session_1";
const projectionVersion = "transcript.projection.v1";
const projectionGeneration = "generation-1";

test("Web accepts current Core-generated blocks including optional presentation facts", () => {
  const samples = JSON.parse(readFileSync(new URL(
    "../../../../api/app_core/generated/transcript_block.samples.json", import.meta.url), "utf8"));
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

test("current queued Run discovery works before a transcript page is available", async () => {
  const paths=[];
  const transport=createWorkspaceTranscriptTransport({request:async path=>{
    paths.push(path);
    return new Response(JSON.stringify({schema:"workspace.transcript.active_agent_run.v1",sessionId,
      agentRun:{agentRunId:"queued-run",status:"queued",streamCursor:"cursor:0"}}));
  }});
  const result=await transport.loadActiveAgentRun({sessionId,sourceHighWater:"0"},new AbortController().signal);
  assert.equal(result.agentRun.agentRunId,"queued-run");
  assert.equal(result.agentRun.status,"queued");
  assert.deepEqual(paths,[`/api/sessions/${sessionId}/transcript/active-agent-run?sourceHighWater=0`]);
});
