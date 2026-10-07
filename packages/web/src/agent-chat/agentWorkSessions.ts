import { parseReferencedSession, type ReferencedSession } from "./agentOutputFeed";
import type { TranscriptInitialInputOrigin } from "../chat/TranscriptBlockContent";

const SESSION_KEYS = "id|workspaceId|agentId|projectId|title|origin|status|deletedAt|isPinned|isUnread|hasActiveAgentRun|updatedAt|initialInputOrigin";
function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("agent_work_sessions_invalid");
  return value as Record<string, unknown>;
}
function exact(value: Record<string, unknown>, keys: string) {
  if (Object.keys(value).sort().join("|") !== keys.split("|").sort().join("|")) throw new Error("agent_work_sessions_invalid");
}
function opaque(value: unknown): value is string { return typeof value === "string" && value.length > 0 && value.trim() === value; }
function inputOrigin(value: unknown): TranscriptInitialInputOrigin | null {
  if (value === null) return null;
  const origin = record(value); exact(origin, "messageId|agentId|agentName");
  if (![origin.messageId, origin.agentId].every(opaque) || typeof origin.agentName !== "string" || !origin.agentName.trim()) throw new Error("session_input_origin_invalid");
  return { messageId: origin.messageId as string, agentId: origin.agentId as string, agentName: origin.agentName };
}

export function parseSessionInputOrigin(value: unknown, workspaceId: string, agentId: string, sessionId: string) {
  const session = record(record(value).session);
  parseReferencedSession(value, workspaceId, sessionId);
  if (session.agentId !== agentId || !Object.hasOwn(session, "initialInputOrigin")) throw new Error("session_input_origin_invalid");
  return inputOrigin(session.initialInputOrigin);
}
export function agentWorkSessionsPath(agentId: string, afterSessionId: string | null = null, limit = 50) {
  if (!opaque(agentId) || !Number.isInteger(limit) || limit < 1 || limit > 100 || (afterSessionId !== null && !opaque(afterSessionId))) throw new Error("agent_work_sessions_request_invalid");
  const query = new URLSearchParams({ limit: String(limit) });
  if (afterSessionId !== null) query.set("afterSessionId", afterSessionId);
  return `/api/agents/${encodeURIComponent(agentId)}/work-sessions?${query}`;
}
export type AgentWorkSessionsPage = Readonly<{ sessions: readonly ReferencedSession[]; nextAfterSessionId: string | null; hasMore: boolean }>;
export function parseAgentWorkSessionsPage(value: unknown, agentId: string, workspaceId: string, afterSessionId: string | null, limit = 50): AgentWorkSessionsPage {
  const page = record(value); exact(page, "schema|agentId|workspaceId|sessions|nextAfterSessionId|hasMore");
  if (page.schema !== "agent.work_sessions.v1" || page.agentId !== agentId || page.workspaceId !== workspaceId
    || !Array.isArray(page.sessions) || page.sessions.length > limit || typeof page.hasMore !== "boolean") throw new Error("agent_work_sessions_invalid");
  const ids = new Set<string>();
  const sessions = page.sessions.map(value => {
    const session = record(value); exact(session, SESSION_KEYS);
    if (!opaque(session.id) || ids.has(session.id) || session.id === afterSessionId || typeof session.origin !== "string"
      || session.deletedAt !== null || typeof session.isPinned !== "boolean" || typeof session.isUnread !== "boolean"
      || (session.projectId !== null && !opaque(session.projectId)) || typeof session.updatedAt !== "string" || !Number.isFinite(Date.parse(session.updatedAt))) throw new Error("agent_work_sessions_invalid");
    ids.add(session.id); inputOrigin(session.initialInputOrigin);
    return parseReferencedSession({ session }, workspaceId, session.id);
  });
  const next = page.nextAfterSessionId;
  if (page.hasMore ? !opaque(next) || next === afterSessionId || next !== sessions.at(-1)?.sessionId : next !== null) throw new Error("agent_work_sessions_cursor_invalid");
  return { sessions, hasMore: page.hasMore, nextAfterSessionId: next as string | null };
}
