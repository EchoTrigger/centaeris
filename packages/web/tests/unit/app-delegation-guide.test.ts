import assert from "node:assert/strict";
import { test } from "node:test";
import vm from "node:vm";
import { buildDelegationGuide } from "../../src/routes/appDelegationGuide.ts";

const base = "https://api.example.test/workspace-api";
const native = { agentId: "root/agent", definitionId: null, workspaceId: "workspace/one" };
const published = { agentId: null, definitionId: "definition/one", workspaceId: "workspace/one" };

test("native quickstart preserves the deployment prefix with a chat resource", () => {
  const guide = buildDelegationGuide(`${base}/`, native);
  assert.equal(guide.kind, "native");
  assert.deepEqual(guide.steps.map(step => step.key), ["createChat", "submitMessage", "readMessages", "readEvents"]);
  assert.equal(guide.steps[0].path, `${base}/api/v1/chats`);
  assert.deepEqual(JSON.parse(guide.steps[0].request!), { agentId: native.agentId, businessUserId: "customer-42" });
  assert.deepEqual(JSON.parse(guide.steps[1].request!), {
    text: "Process this user's request.", fileIds: [],
  });
  assert.deepEqual(guide.steps.map(step => step.requiredScopes), [["assistant:use"], ["messages:submit"], ["sessions:read"], ["sessions:read"]]);
  const acceptance = JSON.parse(guide.steps[1].response);
  assert.equal(acceptance.data.readAt, null);
  assert.deepEqual(Object.keys(acceptance.data).sort(), ["author", "chatId", "createdAt", "fileIds", "id", "readAt", "text"]);
  assert.equal(JSON.parse(guide.steps[2].response).nextCursor, null);
});

test("published assistants use the same chat transport", () => {
  const guide = buildDelegationGuide(base, published);
  assert.equal(guide.kind, "published");
  assert.deepEqual(guide.steps.map(step => [step.method, step.key]),
    buildDelegationGuide(base, native).steps.map(step => [step.method, step.key]));
  assert.deepEqual(JSON.parse(guide.steps[0].request!), { agentId: published.definitionId, businessUserId: "customer-42" });
});

test("guides reject ambiguous targets or unsafe API addresses instead of suggesting a wrong route", () => {
  for (const target of [{ ...native, definitionId: "definition" }, { ...native, agentId: null }, { ...native, workspaceId: "" }]) {
    assert.throws(() => buildDelegationGuide(base, target));
  }
  for (const address of ["", "/api", "javascript:alert(1)", "https://token:secret@example.test", `${base}?token=secret`, `${base}#fragment`]) {
    assert.throws(() => buildDelegationGuide(address, native));
  }
});

test("JavaScript native example creates, submits with a retry key and reads messages", async () => {
  const guide = buildDelegationGuide(base, native);
  const calls: Array<{ url: string, init: { method: string, headers: Record<string, string>, body?: string } }> = [];
  const context = {
    process: { env: { CENTAERIS_TOKEN: "test-only-token" } }, console: { log() {} }, encodeURIComponent,
    fetch: async (url: string, init: { method: string, headers: Record<string, string>, body?: string }) => {
      calls.push({ url, init });
      const data = calls.length === 1 ? { data: { id: "chat/one" } } : { data: [] };
      return { ok: true, status: 200, json: async () => data };
    },
  };
  await vm.runInNewContext(`(async () => {${guide.examples.javascript}\n})()`, context);
  assert.equal(calls.length, 3);
  assert.equal(calls[0].url, guide.steps[0].path);
  assert.equal(calls[0].init.headers["Authorization"], "Bearer test-only-token");
  assert.equal(calls[0].init.headers["X-Centaeris-Business-Branch-Id"], undefined);
  assert.deepEqual(JSON.parse(calls[0].init.body!), { agentId: native.agentId, businessUserId: "customer-42" });
  for (const call of calls) assert.equal(call.init.headers["X-Centaeris-Business-Branch-Id"], undefined);
  assert.equal(calls[1].init.headers["Idempotency-Key"], "business-input-001");
  assert.equal(calls[1].url, `${base}/api/v1/chats/chat%2Fone/messages`);
  assert.deepEqual(JSON.parse(calls[1].init.body!), JSON.parse(guide.steps[1].request!));
  assert.equal(calls[2].url, `${base}/api/v1/chats/chat%2Fone/messages`);
});

test("guide examples use backend environment credentials and never embed grant metadata as a secret", () => {
  const guide = buildDelegationGuide(base, { ...native, accessToken: "never-show-this-token", id: "grant-id" } as typeof native);
  for (const snippet of Object.values(guide.examples)) {
    assert.match(snippet, /CENTAERIS_TOKEN/);
    assert.doesNotMatch(snippet, /never-show-this-token|grant-id|localStorage|document\.cookie/);
  }
});

test("published JavaScript example uses the returned chat and platform model policy", async () => {
  const guide = buildDelegationGuide(base, published);
  const calls: Array<{ url: string, init: { headers: Record<string, string>, body?: string } }> = [];
  await vm.runInNewContext("(async () => {" + guide.examples.javascript + "\n})()", {
    process: { env: { CENTAERIS_TOKEN: "test-only-token" } }, console: { log() {} }, encodeURIComponent,
    fetch: async (url: string, init: { headers: Record<string, string>, body?: string }) => {
      calls.push({ url, init });
      return { ok: true, json: async () => calls.length === 1 ? { data: { id: "published/one" } } : { data: [] } };
    },
  });
  assert.deepEqual(calls.map(call => call.url), [base + "/api/v1/chats",
    base + "/api/v1/chats/published%2Fone/messages", base + "/api/v1/chats/published%2Fone/messages"]);
  assert.deepEqual(JSON.parse(calls[0].init.body!), { agentId: published.definitionId, businessUserId: "customer-42" });
  assert.deepEqual(JSON.parse(calls[1].init.body!), { text: "Process this user's request.", fileIds: [] });
});

test("JavaScript examples stop on rejected requests instead of treating errors as accepted work", async () => {
  for (const target of [native, published]) {
    let calls = 0;
    const guide = buildDelegationGuide(base, target);
    await assert.rejects(vm.runInNewContext("(async () => {" + guide.examples.javascript + "\n})()", {
      process: { env: { CENTAERIS_TOKEN: "test-only-token" } }, console: { log() {} },
      fetch: async () => { calls++; return { ok: false, status: 403 }; },
    }), /HTTP 403/);
    assert.equal(calls, 1);
  }
});
