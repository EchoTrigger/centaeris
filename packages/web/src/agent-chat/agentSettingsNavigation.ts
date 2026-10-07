export function canOpenPrivateAgentSettings(workspace: { id: string; role: string } | null | undefined) {
  return Boolean(workspace?.id && ["owner", "admin", "member"].includes(workspace.role));
}
export function agentSettingsPath(workspaceId: string, agentId?: string) {
  if (!workspaceId) throw new Error("agent_settings_scope_invalid");
  const path = `/w/${encodeURIComponent(workspaceId)}/settings/agents`;
  return agentId === undefined ? path : `${path}?${new URLSearchParams({ agentId })}`;
}
export function agentSettingsSelection(search: string, agentIds: readonly string[]): Readonly<{ kind: "list" } | { kind: "create" } | { kind: "agent"; agentId: string }> {
  const query = new URLSearchParams(search);
  const selected = query.getAll("agentId"); const create = query.getAll("new");
  if (selected.length > 1 || create.length > 1 || (selected.length && create.length)
    || (create.length && create[0] !== "1") || (selected.length && !agentIds.includes(selected[0]))) throw new Error("agent_settings_selection_invalid");
  return selected.length ? { kind: "agent", agentId: selected[0] } : create.length ? { kind: "create" } : { kind: "list" };
}
