import assert from "node:assert/strict";
import { test } from "node:test";
import { parseMessageFiles, parseSessionPreview, sessionPreviewPath } from "../../src/agent-chat/agentPreviewData.ts";
import { projectAgentHistory } from "../../src/agent-chat/agentHistory.ts";

// Synthetic contract rows. Production HTTP DTO consumption is a separate gate.
const sha256 = `sha256:${"a".repeat(64)}`;
const binding = { agentId: "agent", sessionId: "coord", agentRunId: "own-run", inputRefs: ["input"] };
const file = (prefix = "/api/agents/agent/messages/reply/files/input", changes = {}) => ({ inputRef: "input", agentRunId: "own-run", ownerKind: "userLibraryObject", objectRef: "library-file", sourceVersion: "1", sha256, displayName: "Evidence.txt", contentType: "text/plain", sizeBytes: 12,
  previewUrl: `${prefix}/preview?${new URLSearchParams({ sourceVersion: "1", sha256, lang: "zh-CN" })}`,
  downloadUrl: `${prefix}/download?${new URLSearchParams({ sourceVersion: "1", sha256, lang: "zh-CN" })}`, ...changes });
const message = (files = [file()], changes = {}) => ({ schema: "agent.message.files.v1", agentId: "agent", sessionId: "coord", messageId: "reply", agentRunId: "own-run", files, ...changes });
const output = (id = "artifact") => file(`/api/sessions/work/outputs/${id}`, { inputRef: null, ownerKind: "artifact", objectRef: id });
const page = (changes = {}) => ({ schema: "session.preview.v1", sessionId: "work", runFact: null, outputs: [], nextAfterArtifactId: null, hasMore: false, ...changes });

test("session run state requires its own authoritative source and cannot invent waiting or completion", () => {
  for (const fact of [
    { agentRunId: "run", status: "running", sourceType: "hostedRun", eventId: null, createdAtMs: 1 },
    { agentRunId: "run", status: "completed", sourceType: "sessionEvent", eventId: "terminal", createdAtMs: 2 },
    { agentRunId: "run", status: "cancelled", sourceType: "preAdmissionCancellation", eventId: null, createdAtMs: 3 },
  ]) assert.deepEqual(parseSessionPreview(page({ runFact: fact }), "work").runFact, fact);
  const base = { agentRunId: "run", status: "completed", sourceType: "sessionEvent", eventId: "terminal", createdAtMs: 1 };
  for (const changes of [{ sourceType: "hostedRun", eventId: null }, { eventId: null }, { status: "waiting" }, { status: "paused" }, { sourceType: "Read" }, { sourceType: "preAdmissionCancellation" }, { status: "running" }, { createdAtMs: -1 }, { createdAtMs: 1.5 }, { handled: true }]) {
    assert.throws(() => parseSessionPreview(page({ runFact: { ...base, ...changes } }), "work"));
  }
  assert.equal(parseSessionPreview(page(), "work").runFact, null);
  assert.throws(() => parseSessionPreview(page({ hasActiveAgentRun: false }), "work"));
});

test("Outputs retain Artifact identity and require strict scoped pagination", () => {
  const result = parseSessionPreview(page({ outputs: [output()], hasMore: true, nextAfterArtifactId: "artifact" }), "work", null, 1);
  assert.equal(result.outputs[0].ownerKind, "artifact"); assert.equal(result.outputs[0].inputRef, null);
  for (const changes of [{ sessionId: "coord" }, { schema: "unknown" }, { hasMore: true }, { nextAfterArtifactId: "artifact" }, { outputs: [output(), output()] }, { outputs: [{ ...output(), ownerKind: "userLibraryObject" }] }, { outputs: [{ ...output(), inputRef: "guessed-library-id" }] }]) {
    assert.throws(() => parseSessionPreview(page(changes), "work"));
  }
  assert.throws(() => parseSessionPreview(page({ outputs: [output()] }), "work", "artifact"));
  assert.throws(() => parseSessionPreview(page({ outputs: [output()], hasMore: true, nextAfterArtifactId: "artifact" }), "work", null, 50));
  const url = new URL(sessionPreviewPath("work/id", "artifact+id", 1), "https://fixture.invalid");
  assert.equal(url.pathname, "/api/sessions/work%2Fid/preview"); assert.equal(url.searchParams.get("afterArtifactId"), "artifact+id");
  assert.throws(() => sessionPreviewPath("work", null, 101));
});

test("message files bind the original Agent, Session, message, Run and exact committed inputRefs", () => {
  assert.deepEqual(parseMessageFiles(message(), "reply", binding), [file()]);
  assert.deepEqual(parseMessageFiles(message(), "reply", { ...binding, inputRefs: ["input", "input"] }), [file()]);
  for (const changes of [{ agentId: "other" }, { sessionId: "work" }, { messageId: "other" }, { agentRunId: "new-run" }, { files: [] }, { schema: "unknown" }, { file_refs: ["input"] }]) assert.throws(() => parseMessageFiles(message(undefined, changes), "reply", binding));
  for (const changes of [{ inputRef: null }, { inputRef: "other-input" }, { agentRunId: "new-run" }, { sourceVersion: "01" }, { sourceVersion: "0" }, { sourceVersion: "1".repeat(21) }, { sha256: "guessed" }, { sizeBytes: -1 }, { ownerKind: "library" }]) assert.throws(() => parseMessageFiles(message([file(undefined, changes)]), "reply", binding));
});

test("file content URLs cannot substitute an object, latest version, external URL or extra transport field", () => {
  const base = file();
  for (const previewUrl of [base.previewUrl.replace("/input/", "/other-input/"), base.previewUrl.replace("sourceVersion=1", "sourceVersion=2"), base.previewUrl+"&sourceVersion=1", base.previewUrl+"&latest=true", base.previewUrl+"#x", "https://untrusted.invalid"+base.previewUrl, "/api/library/objects/library-file/preview", base.previewUrl.replace("zh-CN", "unknown")]) {
    assert.throws(() => parseMessageFiles(message([{ ...base, previewUrl }]), "reply", binding));
  }
});

test("history carries fileRefs with the originating Run without guessing object metadata", () => {
  const row = { kind: "message" as const, cursor: "position", message: { id: "reply", agentRunId: "own-run", turnId: "turn", toolCallId: "call", sourceSequence: 1, createdAtMs: 1, body: "Whole reply", sessionRefs: [], fileRefs: ["input"] } };
  const view = projectAgentHistory([row], "You", "Agent", new Map(), true, { agentId: "agent", sessionId: "coord" })[0];
  assert.deepEqual(view.fileBinding, binding); assert.equal(view.text, "Whole reply");
  assert.equal(projectAgentHistory([row], "You", "Agent", new Map(), true)[0].fileBinding, undefined);
});
