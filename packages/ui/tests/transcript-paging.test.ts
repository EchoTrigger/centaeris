import { expect, test } from "vitest";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import type {
  TranscriptBlockV1,
  TranscriptPageRpcResponseV1,
  TranscriptPageV1,
  TranscriptPatchV1,
} from "../src/lib/chatBridge";
import {
  catchUpTranscriptPatchesWith,
  DesktopTranscriptView,
  loadTranscriptPageWith,
} from "../src/components/chat/transcriptPaging";

const block = (
  blockId: string,
  blockRevision: string,
  sourceSequence: string,
  body: TranscriptBlockV1["body"],
): TranscriptBlockV1 => ({
  blockId,
  blockRevision,
  orderKey: { sourceSequence, ordinal: 0 },
  body,
});

const page = (
  blocks: TranscriptBlockV1[],
  olderCursor: string | null = null,
): TranscriptPageV1 => ({
  schema: "transcript.page.v1",
  sessionId: "session-1",
  projectionVersion: "transcript.projection.v1",
  projectionGeneration: "generation-1",
  sourceHighWater: "12",
  blocks,
  olderCursor,
  hasOlder: olderCursor !== null,
  resumeCursors: [{ streamId: "session-jsonl.v1", cursor: "12" }],
});

test("Desktop consumes actual Rust-serialized transcript content with explicit nulls", () => {
  const blocks = JSON.parse(execFileSync("cargo", [
    "run", "--locked", "--quiet", "-p", "centaeris-core", "--features", "contract-schema",
    "--example", "transcript_schema", "--", "--samples",
  ], { cwd: fileURLToPath(new URL("../../../", import.meta.url)), encoding: "utf8", timeout: 120_000 })) as TranscriptBlockV1[];
  const messages = DesktopTranscriptView.open(page(blocks)).materializeMessages(false);
  expect(messages[0]).toMatchObject({ role: "user", text: "sample" });
  expect(messages[0].transcriptText).toBeUndefined();
  expect(messages[1]).toMatchObject({ role: "assistant", turn: { finalAnswer: "sample" } });
  expect(messages[2]).toMatchObject({ turn: { chunks: [{ kind: "reasoning", text: "sample" }] } });
  expect(messages[5].transcriptText?.reference).toEqual({ refId: "ref", revision: "1", byteLength: "6" });
}, 130_000);

test.each([
  { inlineContent: "hello", sourceRef: null },
  { inlineContent: "", sourceRef: null },
  { inlineContent: null, sourceRef: { refId: "ref", revision: "1", byteLength: "5" } },
])("accepts exactly one non-null text source: %j", (content) => {
  const body = JSON.parse(JSON.stringify({ kind: "userText", content }));
  expect(() => DesktopTranscriptView.open(page([block("text", "1", "1", body)]))).not.toThrow();
});

test.each([
  { inlineContent: "hello", sourceRef: { refId: "ref", revision: "1", byteLength: "5" } },
  { inlineContent: null, sourceRef: null },
  {},
])("rejects ambiguous or missing text sources: %j", (content) => {
  const body = JSON.parse(JSON.stringify({ kind: "userText", content }));
  expect(() => DesktopTranscriptView.open(page([block("text", "1", "1", body)])))
    .toThrow("must contain exactly one content source");
});

test("long message materialization preserves a readable reference instead of a byte-count placeholder", () => {
  const reference = { refId: "session-event:event-1:text", revision: "1", byteLength: "70000" };
  const view = DesktopTranscriptView.open(page([
    block("long-user", "1", "1", { kind: "userText", content: { sourceRef: reference } }),
  ]));
  const message = view.materializeMessages(false)[0];
  expect(message.transcriptText).toEqual({ sessionId: "session-1", projectionGeneration: "generation-1", reference });
  expect(message.role === "user" && message.text).toBe("");
});

test("paged Desktop view keeps late revisions and hides committed tail behind live overlay", () => {
  const view = DesktopTranscriptView.open(page([
    block("tool:call-8", "1", "8", {
      kind: "tool",
      callId: "call-8",
      toolName: "exec_command",
      status: "running",
      summary: "cargo test",
      summaryRef: null,
      outputRef: null,
    }),
  ], "before-8"));
  const patch: TranscriptPatchV1 = {
    schema: "transcript.patch.v1",
    sessionId: "session-1",
    projectionVersion: "transcript.projection.v1",
    projectionGeneration: "generation-1",
    sourceHighWater: "14",
    streamId: "session-jsonl.v1",
    appliedCursor: "14",
    upserts: [
      block("tool:call-8", "2", "8", {
        kind: "tool",
        callId: "call-8",
        toolName: "exec_command",
        status: "completed",
        summary: "cargo test",
        summaryRef: null,
        outputRef: null,
      }),
      block("assistant-14", "1", "14", {
        kind: "assistantText",
        content: { inlineContent: "done" },
        status: "completed",
      }),
    ],
    removals: [],
  };

  view.applyPatch(patch);

  const liveMessages = view.materializeMessages(true);
  expect(liveMessages).toHaveLength(1);
  expect(liveMessages[0]?.role).toBe("assistant");
  if (liveMessages[0]?.role !== "assistant") throw new Error("missing tool message");
  expect(liveMessages[0].turn.chunks[0]).toMatchObject({
    kind: "task",
    task: { status: "done" },
  });
  expect(view.materializeMessages(false).at(-1)).toMatchObject({
    id: "assistant-14",
    role: "assistant",
    turn: { finalAnswer: "done" },
  });
});

test("older pages stay at the original waterline and cannot overwrite a newer patch", () => {
  const view = DesktopTranscriptView.open(page([], "before-8"));
  view.applyPatch({
    schema: "transcript.patch.v1",
    sessionId: "session-1",
    projectionVersion: "transcript.projection.v1",
    projectionGeneration: "generation-1",
    sourceHighWater: "13",
    streamId: "session-jsonl.v1",
    appliedCursor: "13",
    upserts: [block("assistant-4", "2", "4", {
      kind: "assistantText",
      content: { inlineContent: "new" },
      status: "completed",
    })],
    removals: [],
  });
  view.applyOlderPage(page([block("assistant-4", "1", "4", {
    kind: "assistantText",
    content: { inlineContent: "old" },
    status: "completed",
  })]));

  expect(view.materializeMessages(false)[0]).toMatchObject({
    turn: { finalAnswer: "new" },
  });
  expect(view.olderCursor).toBeNull();
});

test("a block revision preserves unchanged message identities", () => {
  const view = DesktopTranscriptView.open(page([
    block("assistant-7", "1", "7", {
      kind: "assistantText",
      content: { inlineContent: "stable" },
      status: "completed",
    }),
    block("tool:call-8", "1", "8", {
      kind: "tool",
      callId: "call-8",
      toolName: "exec_command",
      status: "running",
      summary: "cargo test",
      summaryRef: null,
      outputRef: null,
    }),
  ]));
  const before = view.materializeMessages(false);

  view.applyPatch({
    schema: "transcript.patch.v1",
    sessionId: "session-1",
    projectionVersion: "transcript.projection.v1",
    projectionGeneration: "generation-1",
    sourceHighWater: "13",
    streamId: "session-jsonl.v1",
    appliedCursor: "13",
    upserts: [block("tool:call-8", "2", "8", {
      kind: "tool",
      callId: "call-8",
      toolName: "exec_command",
      status: "completed",
      summary: "cargo test",
      summaryRef: null,
      outputRef: null,
    })],
    removals: [],
  });
  const after = view.materializeMessages(false);

  expect(after[0]).toBe(before[0]);
  expect(after[1]).not.toBe(before[1]);
});

test("releasing loaded history keeps the tail and committed additions reloadable", () => {
  const view = DesktopTranscriptView.open(page([
    block("assistant-tail", "1", "12", {
      kind: "assistantText",
      content: { inlineContent: "tail" },
      status: "completed",
    }),
  ], "before-tail"));
  view.applyOlderPage(page([
    block("user-old", "1", "1", {
      kind: "userText",
      content: { inlineContent: "old" },
    }),
    block("assistant-old-unmodified", "1", "2", {
      kind: "assistantText",
      content: { inlineContent: "old answer" },
      status: "completed",
    }),
  ]));
  view.applyPatch({
    schema: "transcript.patch.v1",
    sessionId: "session-1",
    projectionVersion: "transcript.projection.v1",
    projectionGeneration: "generation-1",
    sourceHighWater: "13",
    streamId: "session-jsonl.v1",
    appliedCursor: "13",
    upserts: [
      block("user-old", "2", "1", {
        kind: "userText",
        content: { inlineContent: "revised old" },
      }),
      block("assistant-new", "1", "13", {
        kind: "assistantText",
        content: { inlineContent: "new" },
        status: "completed",
      }),
    ],
    removals: [],
  });
  const bytesBeforeRelease = view.managedContentBytes;

  view.releaseLoadedHistory();

  expect(view.materializeMessages(false).map((message) => message.id)).toEqual([
    "user-old",
    "assistant-tail",
    "assistant-new",
  ]);
  expect(view.olderCursor).toBe("before-tail");
  expect(view.managedContentBytes).toBeGreaterThan(0);
  expect(view.managedContentBytes).toBeLessThan(bytesBeforeRelease);
});

test("page polling freezes the generation and source waterline", async () => {
  const requests: unknown[] = [];
  const responses: TranscriptPageRpcResponseV1[] = [
    {
      schema: "transcript.page.rpc.v1",
      projectionVersion: "transcript.projection.v1",
      projectionGeneration: "generation-1",
      projectedSourceHighWater: "4",
      targetSourceHighWater: "12",
      targetReached: false,
      page: null,
    },
    {
      schema: "transcript.page.rpc.v1",
      projectionVersion: "transcript.projection.v1",
      projectionGeneration: "generation-1",
      projectedSourceHighWater: "12",
      targetSourceHighWater: "12",
      targetReached: true,
      page: page([]),
    },
  ];
  const loaded = await loadTranscriptPageWith(
    { sessionId: "session-1" },
    async (request) => {
      requests.push(request);
      const response = responses.shift();
      if (!response) throw new Error("unexpected request");
      return response;
    },
    async () => {},
  );

  expect(loaded.sourceHighWater).toBe("12");
  expect(requests).toEqual([
    { sessionId: "session-1" },
    {
      sessionId: "session-1",
      projectionGeneration: "generation-1",
      sourceHighWater: "12",
    },
  ]);
});

test("patch catch-up advances through the fixed generation", async () => {
  const view = DesktopTranscriptView.open(page([]));
  const requests: unknown[] = [];
  const changed = await catchUpTranscriptPatchesWith(
    view,
    async (request) => {
      requests.push(request);
      return {
        schema: "transcript.patch.rpc.v1",
        projectionVersion: "transcript.projection.v1",
        projectionGeneration: "generation-1",
        projectedSourceHighWater: "14",
        targetSourceHighWater: "14",
        targetReached: true,
        patches: [{
          schema: "transcript.patch.v1",
          sessionId: "session-1",
          projectionVersion: "transcript.projection.v1",
          projectionGeneration: "generation-1",
          sourceHighWater: "14",
          streamId: "session-jsonl.v1",
          appliedCursor: "14",
          upserts: [block("assistant-14", "1", "14", {
            kind: "assistantText",
            content: { inlineContent: "done" },
            status: "completed",
          })],
          removals: [],
        }],
        nextSourceHighWater: "14",
        hasMore: false,
      };
    },
    async () => {},
  );

  expect(changed).toBe(true);
  expect(view.currentSourceHighWater).toBe("14");
  expect(requests).toEqual([{
    sessionId: "session-1",
    projectionGeneration: "generation-1",
    afterSourceHighWater: "12",
  }]);
});


test("history restores tool path, output bounds and latency from Core display facts", () => {
  const item = block("tool:c", "2", "1", { kind: "tool", callId: "c", toolName: "read", status: "completed", summary: "Read file", summaryRef: null, outputRef: null });
  item.presentation = { agentRunId: "r", sourceType: "tool_result", observedAtMs: 2000, displayTarget: "README.md", durationMs: 25,
    operation: { callId: "c", toolName: "read", status: "completed", resultState: "successWithOutput", path: "README.md", startLine: 1, endLine: 5, contentStartByte: 12, contentByteLength: 30 } };
  expect(DesktopTranscriptView.open(page([item])).materializeMessages(false)[0]).toMatchObject({ turn: { projectionRunId: "r", chunks: [{ task: {
    displayTarget: "README.md", durationMs: 25, operations: [{ path: "README.md", contentStartByte: 12, contentByteLength: 30 }],
  } }] } });
});
