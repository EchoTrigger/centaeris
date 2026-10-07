import assert from "node:assert/strict";
import { test } from "node:test";
import vm from "node:vm";
import { buildDelegationGuide } from "../../src/routes/appDelegationGuide.ts";
import { parsePreparedAgentInputAcceptance, prepareAgentInputSubmission } from "../../src/agent-chat/preparedAgentInput.ts";

const base = "https://api.example.test/workspace-api";
const native = { agentId: "root/agent", definitionId: null, workspaceId: "workspace/one" };
const published = { agentId: null, definitionId: "definition/one", workspaceId: "workspace/one" };

test("native quickstart preserves the configured deployment prefix and exact input contract", () => {
  const guide = buildDelegationGuide(`${base}/`, native);
  assert.equal(guide.kind, "native");
  assert.deepEqual(guide.steps.map(step => step.key), ["resolveBranch", "submitInput", "readInputs", "readMessages"]);
  assert.equal(guide.steps[0].path, `${base}/api/agents/root%2Fagent/business-branches/resolve`);
  assert.deepEqual(JSON.parse(guide.steps[0].request!), { businessUserId: "customer-42" });
  assert.deepEqual(JSON.parse(guide.steps[1].request!), {
    schema: "agent.input.submit.v1", inputId: "business-input-001", body: "Process this user's request.", attachmentRefs: [],
  });
  assert.deepEqual(guide.steps.map(step => step.requiredScopes), [["assistant:use"], ["messages:submit"], ["sessions:read"], ["sessions:read"]]);
  const acceptance = JSON.parse(guide.steps[1].response);
  assert.equal(acceptance.schema, "agent.input.accepted.v1");
  assert.equal(acceptance.input.read, null);
  assert.deepEqual(Object.keys(acceptance.input).sort(), ["attachments", "body", "createdAtMs", "inputId", "read", "sequence"]);
  const submission = prepareAgentInputSubmission("business-input-001", "Process this user's request.");
  assert.deepEqual(JSON.parse(guide.steps[1].request!), submission);
  parsePreparedAgentInputAcceptance(acceptance, { agentId: "returned-agent-id", sessionId: "returned-session-id" }, submission);
  assert.equal(JSON.parse(guide.steps[3].response).nextAfterSequence, null);
});

test("published quickstart has instance and Session receipts without native branch fields", () => {
  const guide = buildDelegationGuide(base, published);
  assert.equal(guide.kind, "published");
  assert.deepEqual(guide.steps.map(step => step.key), ["obtainInstance", "selectModel", "createSession", "submitMessage", "readEvents"]);
  assert.equal(guide.steps[0].path, `${base}/api/workspaces/workspace%2Fone/available-agent-definitions/definition%2Fone/instance`);
  assert.deepEqual(JSON.parse(guide.steps[0].request!), {});
  assert.equal(guide.steps[1].path, `${base}/api/models`);
  assert.deepEqual(JSON.parse(guide.steps[2].request!), { operationId: "create-session-001", agentId: "returned-agent-id" });
  assert.equal(JSON.parse(guide.steps[2].response).command, "createSession");
  assert.equal(JSON.parse(guide.steps[3].response).command, "submitMessage");
  assert.deepEqual(guide.steps[4].requiredScopes, ["events:read"]);
  for (const snippet of Object.values(guide.examples)) {
    assert.doesNotMatch(snippet, /business-branches|X-Centaeris-Business-Branch-Id|businessUserId/);
  }
});

test("guides reject ambiguous targets or unsafe API addresses instead of suggesting a wrong route", () => {
  for (const target of [{ ...native, definitionId: "definition" }, { ...native, agentId: null }, { ...native, workspaceId: "" }]) {
    assert.throws(() => buildDelegationGuide(base, target));
  }
  for (const address of ["", "/api", "javascript:alert(1)", "https://token:secret@example.test", `${base}?token=secret`, `${base}#fragment`]) {
    assert.throws(() => buildDelegationGuide(address, native));
  }
});

test("JavaScript native example sends authenticated stable identity, branch header and exact persisted input", async () => {
  const guide = buildDelegationGuide(base, native);
  const calls: Array<{ url: string, init: { method: string, headers: Record<string, string>, body?: string } }> = [];
  const context = {
    process: { env: { CENTAERIS_TOKEN: "test-only-token" } }, console: { log() {} }, encodeURIComponent,
    fetch: async (url: string, init: { method: string, headers: Record<string, string>, body?: string }) => {
      calls.push({ url, init });
      const data = calls.length === 1 ? { agentId: "leaf/one", branchId: "branch/one", sessionId: "coord" }
        : { schema: calls.length === 2 ? "agent.input.accepted.v1" : calls.length === 3 ? "agent.inputs.v1" : "agent.messages.v1" };
      return { ok: true, status: 200, json: async () => data };
    },
  };
  await vm.runInNewContext(`(async () => {${guide.examples.javascript}\n})()`, context);
  assert.equal(calls.length, 4);
  assert.equal(calls[0].url, guide.steps[0].path);
  assert.equal(calls[0].init.headers["Authorization"], "Bearer test-only-token");
  assert.equal(calls[0].init.headers["X-Centaeris-Business-Branch-Id"], undefined);
  assert.deepEqual(JSON.parse(calls[0].init.body!), { businessUserId: "customer-42" });
  for (const call of calls.slice(1)) assert.equal(call.init.headers["X-Centaeris-Business-Branch-Id"], "branch/one");
  assert.equal(calls[1].url, `${base}/api/agents/leaf%2Fone/inputs`);
  assert.deepEqual(JSON.parse(calls[1].init.body!), JSON.parse(guide.steps[1].request!));
  assert.equal(calls[2].url, `${base}/api/agents/leaf%2Fone/inputs?afterSequence=0&limit=50`);
  assert.equal(calls[3].url, `${base}/api/agents/leaf%2Fone/messages?afterSequence=0&limit=50`);
});

test("guide examples use backend environment credentials and never embed grant metadata as a secret", () => {
  const guide = buildDelegationGuide(base, { ...native, accessToken: "never-show-this-token", id: "grant-id" } as typeof native);
  for (const snippet of Object.values(guide.examples)) {
    assert.match(snippet, /CENTAERIS_TOKEN/);
    assert.doesNotMatch(snippet, /never-show-this-token|grant-id|localStorage|document\.cookie/);
  }
});

test("JavaScript published example uses returned model, Session and Run identities before opening SSE", async () => {
  const guide = buildDelegationGuide(base, published);
  const calls: Array<{ url: string, init: { method?: string, headers: Record<string, string>, body?: string } }> = [];
  const payloads = [
    { agent: { id: "instance/one" } }, { models: [{ id: "selected-model" }] },
    { sessionId: "session/one" }, { agentRunId: "run/one" },
  ];
  const context = {
    process: { env: { CENTAERIS_TOKEN: "test-only-token" } }, console: { log() {} }, encodeURIComponent, TextDecoder,
    fetch: async (url: string, init: { method?: string, headers: Record<string, string>, body?: string }) => {
      calls.push({ url, init });
      return calls.length <= payloads.length
        ? { ok: true, status: 200, json: async () => payloads[calls.length - 1] }
        : { ok: true, status: 200, body: { getReader: () => ({ read: async () => ({ done: true }) }) } };
    },
  };
  await vm.runInNewContext("(async () => {" + guide.examples.javascript + "\n})()", context);
  assert.equal(calls.length, 5);
  assert.deepEqual(calls.map(call => call.url), [
    guide.steps[0].path, base + "/api/models", base + "/api/workspaces/workspace%2Fone/sessions",
    base + "/api/workspaces/workspace%2Fone/sessions/session%2Fone/messages",
    base + "/api/sessions/session%2Fone/agent-runs/run%2Fone/events",
  ]);
  assert.deepEqual(JSON.parse(calls[2].init.body!), { operationId: "create-session-001", agentId: "instance/one" });
  assert.deepEqual(JSON.parse(calls[3].init.body!), { operationId: "submit-message-001", text: "Hello", modelConfigRef: "selected-model" });
  assert.equal(calls[4].init.headers["Accept"], "text/event-stream");
  for (const call of calls) {
    assert.equal(call.init.headers["Authorization"], "Bearer test-only-token");
    assert.equal(call.init.headers["X-Centaeris-Business-Branch-Id"], undefined);
  }
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
