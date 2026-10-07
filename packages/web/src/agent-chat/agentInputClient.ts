import { parsePreparedAgentInputAcceptance, prepareAgentInputSubmission, type PreparedAgentInputBinding, type PreparedAgentInputSubmission } from "./preparedAgentInput.ts";

type Storage = Pick<globalThis.Storage, "getItem" | "setItem" | "removeItem">;
export type AgentInputScope = Readonly<{ userId: string; workspaceId: string; agentId: string }>;
type Pending = Readonly<{ binding: PreparedAgentInputBinding; submission: PreparedAgentInputSubmission }>;
type Request = (path: string, options?: RequestInit) => Promise<unknown>;
function opaque(value: unknown): value is string { return typeof value === "string" && Boolean(value) && value.trim() === value; }
export function parseAgentInputBinding(value: unknown, agentId: string): PreparedAgentInputBinding {
  const row = value as Record<string, unknown>;
  if (!row || typeof row !== "object" || Array.isArray(row) || Object.keys(row).sort().join("|") !== "agentId|schema|sessionId"
    || row.schema !== "agent.coordination_session.v1" || !opaque(agentId) || row.agentId !== agentId || !opaque(row.sessionId)) throw new Error("agent_input_binding_invalid");
  return { agentId, sessionId: row.sessionId };
}

// The current tab retains exact uncertain input across navigation and reload.
// Stored binding/acceptance is never authority: every retry goes through HTTP.
export function createAgentInputClient(scope: AgentInputScope, request: Request, storage?: Storage) {
  if (![scope.userId, scope.workspaceId, scope.agentId].every(opaque)) throw new Error("agent_input_scope_invalid");
  const key = `centaeris.pendingAgentInput.v1:${JSON.stringify([scope.userId, scope.workspaceId, scope.agentId])}`;
  const path = `/api/agents/${encodeURIComponent(scope.agentId)}`;
  // Access can be denied before even calling getItem. Keep that failure inside
  // the caller's preparation/submit error handling, rather than React render.
  const inputStorage = () => storage ?? sessionStorage;
  const pending = (): Pending | null => {
    const raw = inputStorage().getItem(key);
    if (raw === null) return null;
    const row = JSON.parse(raw);
    if (!row || Object.keys(row).sort().join("|") !== "binding|submission" || !row.binding || Object.keys(row.binding).sort().join("|") !== "agentId|sessionId"
      || row.binding.agentId !== scope.agentId || !opaque(row.binding.sessionId) || !row.submission
      || row.submission.schema !== "agent.input.submit.v1") throw new Error("agent_input_storage_invalid");
    const keys = Object.keys(row.submission).sort().join("|");
    // Explicit forward reconstruction of a text-only local retry record. The
    // server wire parser remains strict; keep the original uncertain identity.
    if (keys !== "body|inputId|schema" && keys !== "attachmentRefs|body|inputId|schema") throw new Error("agent_input_storage_invalid");
    const migrated = { binding: { agentId: scope.agentId, sessionId: row.binding.sessionId }, submission: prepareAgentInputSubmission(row.submission.inputId, row.submission.body, keys === "body|inputId|schema" ? [] : row.submission.attachmentRefs) };
    if (keys === "body|inputId|schema") inputStorage().setItem(key, JSON.stringify(migrated));
    return migrated;
  };
  return {
    pending,
    loadBinding: async (signal?: AbortSignal) => parseAgentInputBinding(await request(`${path}/coordination-session`, { signal }), scope.agentId),
    createBinding: async (signal?: AbortSignal) => parseAgentInputBinding(await request(`${path}/coordination-session`, { method: "POST", body: "{}", signal }), scope.agentId),
    submit: async (binding: PreparedAgentInputBinding, body: string, signal?: AbortSignal, expectedInputId?: string, attachmentRefs: readonly string[] = []) => {
      if (binding.agentId !== scope.agentId || !opaque(binding.sessionId)) throw new Error("agent_input_binding_invalid");
      let item = pending();
      if (expectedInputId && item?.submission.inputId !== expectedInputId) throw new Error("agent_input_result_stale");
      const submission = prepareAgentInputSubmission(item?.submission.inputId ?? `input:${crypto.randomUUID()}`, body, attachmentRefs);
      if (item && (item.binding.sessionId !== binding.sessionId || item.submission.body !== body || JSON.stringify(item.submission.attachmentRefs) !== JSON.stringify(submission.attachmentRefs))) throw new Error("agent_input_pending_changed");
      if (!item) {
        item = { binding, submission };
        // Storage failure stops before the server could accept the input.
        inputStorage().setItem(key, JSON.stringify(item));
      }
      const accepted = parsePreparedAgentInputAcceptance(await request(`${path}/inputs`, { method: "POST", body: JSON.stringify(item.submission), signal }), binding, item.submission);
      if (pending()?.submission.inputId !== item.submission.inputId) throw new Error("agent_input_result_stale");
      inputStorage().removeItem(key);
      return accepted;
    },
  };
}
