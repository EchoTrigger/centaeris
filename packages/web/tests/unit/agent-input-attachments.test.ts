import assert from "node:assert/strict";
import { test } from "node:test";
import { prepareAgentInputSubmission, parsePreparedAgentInputAcceptance } from "../../src/agent-chat/preparedAgentInput.ts";
import { createAgentInputClient } from "../../src/agent-chat/agentInputClient.ts";
import { createAgentInputDraftStore } from "../../src/agent-chat/agentInputDraft.ts";

const scope = { userId: "user", workspaceId: "workspace", agentId: "agent" };
const binding = { agentId: "agent", sessionId: "coord" };
const image = { inputRef: "image", displayName: "测试.png", contentType: "image/png" };
const store = () => { const rows = new Map<string, string>(); return { getItem: (key: string) => rows.get(key) ?? null, setItem: (key: string, value: string) => { rows.set(key, value); }, removeItem: (key: string) => { rows.delete(key); } }; };
const acceptance = (body: string, attachments = [image]) => ({ schema: "agent.input.accepted.v1", ...binding, input: { inputId: "input", sequence: 1, createdAtMs: 1, body, attachments, read: null } });

test("image-only input carries explicit references and acceptance must match the attachment set", () => {
  const submission = prepareAgentInputSubmission("input", "", ["image"]);
  assert.deepEqual(submission.attachmentRefs, ["image"]);
  assert.deepEqual(parsePreparedAgentInputAcceptance(acceptance(""), binding, submission).input.attachments, [image]);
  assert.throws(() => parsePreparedAgentInputAcceptance(acceptance("", []), binding, submission));
  assert.throws(() => prepareAgentInputSubmission("input", "", []));
  assert.throws(() => prepareAgentInputSubmission("input", "text", ["image", "image"]));
});

test("attachment drafts persist within one user/workspace/Agent and clear only after consumption", () => {
  const storage = store(); const drafts = createAgentInputDraftStore(scope, storage);
  assert.equal(drafts.write("", [image]), true);
  assert.deepEqual(drafts.read().attachments, [image]);
  assert.deepEqual(createAgentInputDraftStore({ ...scope, agentId: "other" }, storage).read().attachments, []);
  drafts.write("", []); assert.deepEqual(drafts.read().attachments, []);
});

test("existing text draft and uncertain identity forward-migrate without losing text or changing inputId", () => {
  const storage = store(); const suffix = JSON.stringify([scope.userId, scope.workspaceId, scope.agentId]);
  storage.setItem(`centaeris.agentInputDraft.v1:${suffix}`, JSON.stringify({ schema: "agent.input.draft.v1", body: " exact old draft " }));
  assert.equal(createAgentInputDraftStore(scope, storage).read().body, " exact old draft ");
  assert.deepEqual(createAgentInputDraftStore(scope, storage).read().attachments, []);
  storage.setItem(`centaeris.pendingAgentInput.v1:${suffix}`, JSON.stringify({ binding, submission: { schema: "agent.input.submit.v1", inputId: "retained", body: " old pending " } }));
  const pending = createAgentInputClient(scope, async () => { throw new Error("no request"); }, storage).pending();
  assert.equal(pending?.submission.inputId, "retained"); assert.equal(pending?.submission.body, " old pending ");
  assert.deepEqual(pending?.submission.attachmentRefs, []);
});

test("an uncertain attachment submission freezes both text and refs and retries the same identity", async () => {
  const storage = store(); const posts: unknown[] = [];
  const client = createAgentInputClient(scope, async (_path, options) => { posts.push(JSON.parse(String(options?.body))); throw new Error("response lost"); }, storage);
  await assert.rejects(client.submit(binding, "", undefined, undefined, ["image"]));
  await assert.rejects(client.submit(binding, "", undefined, undefined, ["different"]), /pending_changed/);
  await assert.rejects(client.submit(binding, "", undefined, undefined, ["image"]));
  assert.equal(posts.length, 2); assert.deepEqual(posts[0], posts[1]);
});
