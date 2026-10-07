export type AgentModelSettings = Readonly<{ schema: "agent.model_settings.v1"; agentId: string; modelConfigRef: string | null; thinkingMode: string | null; status: "configured" | "unconfigured" | "unavailable" }>;
export type AgentModelChoice = Readonly<{ id: string; displayName: string; providerId: string | null; providerDisplayName: string | null; modelName: string; contextTokens: number; maxOutputTokens: number; thinkingMode: string | null; thinkingModes: readonly string[] }>;
function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("agent_model_settings_response_invalid");
  return value as Record<string, unknown>;
}
function exact(value: Record<string, unknown>, fields: string) {
  if (Object.keys(value).sort().join("|") !== fields.split("|").sort().join("|")) throw new Error("agent_model_settings_response_invalid");
}
function opaque(value: unknown): value is string { return typeof value === "string" && Boolean(value) && value.trim() === value; }
export function parseAgentModelSettings(value: unknown, agentId: string): AgentModelSettings {
  const settings = record(value);
  exact(settings, "schema|agentId|modelConfigRef|thinkingMode|status");
  if (!opaque(agentId) || settings.schema !== "agent.model_settings.v1" || settings.agentId !== agentId
    || !["configured", "unconfigured", "unavailable"].includes(settings.status as string)
    || (settings.thinkingMode !== null && !opaque(settings.thinkingMode))
    || (settings.status === "unconfigured" ? settings.modelConfigRef !== null || settings.thinkingMode !== null : !opaque(settings.modelConfigRef))) throw new Error("agent_model_settings_response_invalid");
  return structuredClone(settings) as AgentModelSettings;
}
export function prepareAgentModelUpdate(modelConfigRef: string | null, thinkingMode: string | null) {
  if ((modelConfigRef !== null && !opaque(modelConfigRef)) || (thinkingMode !== null && !opaque(thinkingMode)) || (modelConfigRef === null && thinkingMode !== null)) throw new Error("agent_model_settings_invalid");
  return { schema: "agent.model_settings.update.v1" as const, modelConfigRef, thinkingMode };
}
export function createAgentModelSettingsClient(agentId: string, request: (path: string, options?: RequestInit) => Promise<unknown>) {
  if (!opaque(agentId)) throw new Error("agent_model_settings_invalid");
  const path = `/api/agents/${encodeURIComponent(agentId)}/model-settings`;
  return {
    load: async (signal?: AbortSignal) => parseAgentModelSettings(await request(path, { signal }), agentId),
    update: async (modelConfigRef: string | null, thinkingMode: string | null, signal?: AbortSignal) => {
      const value = parseAgentModelSettings(await request(path, { method: "PATCH", body: JSON.stringify(prepareAgentModelUpdate(modelConfigRef, thinkingMode)), signal }), agentId);
      if (value.modelConfigRef !== modelConfigRef || (thinkingMode !== null && value.thinkingMode !== thinkingMode)) throw new Error("agent_model_settings_update_mismatch");
      return value;
    },
  };
}
export function parseAgentModelCatalog(value: unknown): readonly AgentModelChoice[] {
  const envelope = record(value); exact(envelope, "models");
  if (!Array.isArray(envelope.models)) throw new Error("agent_model_catalog_invalid");
  const ids = new Set<string>();
  for (const raw of envelope.models) {
    const model = record(raw);
    exact(model, "id|displayName|providerId|providerDisplayName|modelName|contextTokens|maxOutputTokens|thinkingMode|thinkingModes");
    if (!opaque(model.id) || ids.has(model.id) || typeof model.displayName !== "string" || !opaque(model.modelName)
      || (model.providerId !== null && !opaque(model.providerId)) || (model.providerDisplayName !== null && typeof model.providerDisplayName !== "string")
      || !Number.isSafeInteger(model.contextTokens) || Number(model.contextTokens) <= 0 || !Number.isSafeInteger(model.maxOutputTokens) || Number(model.maxOutputTokens) <= 0
      || (model.thinkingMode !== null && !opaque(model.thinkingMode)) || !Array.isArray(model.thinkingModes) || !model.thinkingModes.every(opaque)
      || new Set(model.thinkingModes).size !== model.thinkingModes.length) throw new Error("agent_model_catalog_invalid");
    ids.add(model.id);
  }
  return structuredClone(envelope.models) as AgentModelChoice[];
}
export function groupAgentModels(models: readonly AgentModelChoice[], fallbackLabel: string) {
  const groups = new Map<string, { provider: string; label: string; models: AgentModelChoice[] }>();
  for (const model of models) {
    const provider = model.providerId ?? "builtin";
    let group = groups.get(provider);
    if (!group) { group = { provider, label: model.providerDisplayName || fallbackLabel, models: [] }; groups.set(provider, group); }
    group.models.push(model);
  }
  return [...groups.values()];
}
