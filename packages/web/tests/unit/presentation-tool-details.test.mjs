import assert from "node:assert/strict";
import test from "node:test";
import { TranscriptToolOperationRegistry } from "../../src/chat/transcriptToolOperations.ts";
const call = { kind: "call", callId: "call", toolName: "edit", normalizedInput: { path: "file.ts", command: "synthetic command" }, resultState: null, operations: [], previewTruncated: false, sourceEventId: "call-event" };
const result = { ...call, kind: "result", normalizedInput: null, resultState: "succeeded", operations: [{ callId: "call", toolName: "edit", status: "completed", resultState: "succeeded", path: "file.ts", diffPreview: "+synthetic" }], previewTruncated: true, sourceEventId: "result-event" };

test("presentation details preserve commands and mutation diffs without a Core event envelope", () => {
  const registry = new TranscriptToolOperationRegistry();
  registry.applyPresentationDetail(call); registry.applyPresentationDetail(result);
  const record = registry.getSnapshot().records.get("call");
  assert.equal(record.call.normalizedInput.command, "synthetic command");
  assert.equal(record.operations[0].diffPreview, "+synthetic");
  assert.equal(record.status, "completed");
  assert.equal(record.previewTruncated, true);
  assert.equal(record.detailSourceEventId, "result-event");
});

test("replay cannot rewind a completed tool or emit duplicate registry notifications", () => {
  const registry = new TranscriptToolOperationRegistry(); let notifications = 0;
  registry.subscribe(() => notifications++);
  registry.applyPresentationDetail(call); registry.applyPresentationDetail(result);
  const snapshot = registry.getSnapshot();
  registry.applyPresentationDetail({ ...call, normalizedInput: { command: "synthetic command", path: "file.ts" } });
  registry.applyPresentationDetail(result);
  assert.equal(registry.getSnapshot(), snapshot);
  assert.equal(notifications, 2);
  assert.throws(() => registry.applyPresentationDetail({ ...result, resultState: "failed" }), /conflicting/);
  registry.clear(); registry.applyPresentationDetail(call);
  assert.equal(registry.getSnapshot().records.get("call").status, "running");
});

test("operation bindings remain validated and failed mutations cannot carry successful diffs", () => {
  const registry = new TranscriptToolOperationRegistry();
  assert.throws(() => registry.applyPresentationDetail({ ...result, operations: [{ ...result.operations[0], callId: "foreign" }] }), /binding/);
  assert.throws(() => registry.applyPresentationDetail({ ...result, operations: [{ ...result.operations[0], resultState: "failed" }] }), /failed mutation/);
  registry.applyPresentationDetail({ ...result, resultState: "failed", operations: [] });
  assert.equal(registry.getSnapshot().records.get("call").status, "failed");
});
