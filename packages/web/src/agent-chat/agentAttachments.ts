import { parseAgentInputAttachments, type AgentInputAttachment, type PreparedAgentInputBinding } from "./preparedAgentInput.ts";

type Request = (path: string, options?: RequestInit) => Promise<unknown>;
function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("agent_attachment_response_invalid");
  return value as Record<string, unknown>;
}
const opaque = (value: unknown): value is string => typeof value === "string" && Boolean(value) && value.trim() === value;
function attachment(value: unknown): AgentInputAttachment {
  const row = record(value);
  if (!opaque(row.id) || !opaque(row.displayName) || !opaque(row.contentType) || !["userLibraryObject", "sourceObject", "artifact"].includes(String(row.assetKind))) throw new Error("agent_attachment_response_invalid");
  return { inputRef: row.id, displayName: row.displayName, contentType: row.contentType };
}
export function mergeAgentAttachments(existing: readonly AgentInputAttachment[], incoming: readonly AgentInputAttachment[]) {
  const byRef = new Map(existing.map(item => [item.inputRef, item]));
  for (const item of incoming) {
    const previous = byRef.get(item.inputRef);
    if (previous && JSON.stringify(previous) !== JSON.stringify(item)) throw new Error("agent_attachment_identity_changed");
    byRef.set(item.inputRef, item);
  }
  return parseAgentInputAttachments([...byRef.values()].sort((a, b) => a.inputRef < b.inputRef ? -1 : a.inputRef > b.inputRef ? 1 : 0));
}
export async function uploadAgentAttachments(binding: PreparedAgentInputBinding, files: readonly File[], request: Request): Promise<readonly AgentInputAttachment[]> {
  if (!files.length || files.length > 50) throw new Error("agent_attachment_batch_invalid");
  const body = new FormData(); files.forEach(file => body.append("files", file));
  const result = record(await request(`/api/sessions/${encodeURIComponent(binding.sessionId)}/uploads`, { method: "POST", body }));
  if (!Array.isArray(result.assets) || !Array.isArray(result.libraryObjects) || result.assets.length !== files.length || result.libraryObjects.length !== files.length) throw new Error("agent_attachment_response_invalid");
  const attached = result.assets.map((value, index) => {
    const link = record(value); const asset = record(link.asset); const library = record((result.libraryObjects as unknown[])[index]);
    if (link.assetKind !== "userLibraryObject" || asset.id !== library.id || link.displayName !== files[index].name || library.displayName !== files[index].name) throw new Error("agent_attachment_response_invalid");
    return attachment(value);
  });
  if (new Set(attached.map(item => item.inputRef)).size !== attached.length) throw new Error("agent_attachment_response_invalid");
  return mergeAgentAttachments([], attached);
}
