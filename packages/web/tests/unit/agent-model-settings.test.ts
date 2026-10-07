import assert from "node:assert/strict";
import { test } from "node:test";
import { parseAgentModelSettings, prepareAgentModelUpdate, createAgentModelSettingsClient, parseAgentModelCatalog, groupAgentModels } from "../../src/agent-chat/agentModelSettings.ts";

const settings = (overrides = {}) => ({ schema: "agent.model_settings.v1", agentId: "agent", modelConfigRef: "model-exact", thinkingMode: "high", status: "configured", ...overrides });
const model = (overrides = {}) => ({ id: "model-exact", displayName: "Exact model", providerId: "provider", providerDisplayName: "Provider", modelName: "native-name", contextTokens: 1000, maxOutputTokens: 100, thinkingMode: "high", thinkingModes: ["low", "high"], ...overrides });

test("persistent settings retain exact model IDs, effective effort and explicit unconfigured/unavailable states", () => {
  assert.deepEqual(parseAgentModelSettings(settings(), "agent"), settings());
  assert.equal(parseAgentModelSettings(settings({ modelConfigRef: null, thinkingMode: null, status: "unconfigured" }), "agent").status, "unconfigured");
  assert.equal(parseAgentModelSettings(settings({ status: "unavailable" }), "agent").modelConfigRef, "model-exact");
  assert.equal(parseAgentModelSettings(settings({ thinkingMode: null }), "agent").thinkingMode, null);
  for (const value of [settings({ agentId: "other" }), settings({ schema: "unknown" }), settings({ status: "unknown" }), settings({ status: "unconfigured" }), settings({ modelConfigRef: null }), settings({ activeRunId: "run" })]) assert.throws(() => parseAgentModelSettings(value, "agent"));
});

test("explicit update/clear requests have only frozen fields; null effort is sent unchanged for server resolution", () => {
  assert.deepEqual(prepareAgentModelUpdate("model-exact", null), { schema: "agent.model_settings.update.v1", modelConfigRef: "model-exact", thinkingMode: null });
  assert.deepEqual(prepareAgentModelUpdate(null, null), { schema: "agent.model_settings.update.v1", modelConfigRef: null, thinkingMode: null });
  assert.throws(() => prepareAgentModelUpdate(null, "high"));
  assert.throws(() => prepareAgentModelUpdate("", null));
  assert.throws(() => prepareAgentModelUpdate("model-exact", ""));
});

test("settings GET/PATCH use exact Agent scope and accepted effective response, never a Run or input endpoint", async () => {
  const calls = [];
  const client = createAgentModelSettingsClient("agent", async (path, options) => { calls.push({ path, options }); return settings({ thinkingMode: "low" }); });
  assert.equal((await client.load()).thinkingMode, "low");
  assert.equal((await client.update("model-exact", null)).thinkingMode, "low");
  assert.equal(calls[0].path, "/api/agents/agent/model-settings");
  assert.equal(calls[1].options.method, "PATCH");
  assert.deepEqual(JSON.parse(calls[1].options.body), prepareAgentModelUpdate("model-exact", null));
  assert.equal(calls.every(call => call.path.endsWith("/model-settings")), true);
});

test("settings errors and changed response scope propagate without fallback or optimistic success", async () => {
  const denied = createAgentModelSettingsClient("agent", async () => { throw new Error("agent_model_not_available"); });
  await assert.rejects(denied.update("model-exact", "high"), /agent_model_not_available/);
  const wrong = createAgentModelSettingsClient("agent", async () => settings({ agentId: "other" }));
  await assert.rejects(wrong.load());
  const changed = createAgentModelSettingsClient("agent", async () => settings({ modelConfigRef: "substituted" }));
  await assert.rejects(changed.update("model-exact", null), /agent_model_settings_update_mismatch/);
  const changedEffort = createAgentModelSettingsClient("agent", async () => settings({ thinkingMode: "low" }));
  await assert.rejects(changedEffort.update("model-exact", "high"), /agent_model_settings_update_mismatch/);
});

test("catalog grouping preserves exact choices but makes no first-entry or provider/name substitution", () => {
  const catalog = parseAgentModelCatalog({ models: [model(), model({ id: "second", displayName: "Second" })] });
  const groups = groupAgentModels(catalog, "Models");
  assert.deepEqual(groups[0].models.map(item => item.id), ["model-exact", "second"]);
  assert.equal("selectedId" in groups[0], false);
  assert.equal(catalog.find(item => item.id === "saved-unavailable"), undefined);
  assert.throws(() => parseAgentModelCatalog({ models: [model(), model()] }));
  assert.throws(() => parseAgentModelCatalog({ models: [model({ thinkingModes: ["high", "high"] })] }));
});
