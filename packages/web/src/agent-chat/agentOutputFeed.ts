import type { AgentMessageView, AgentSessionReferenceView } from "./agentChatViewTypes";

// Exact existing agent.messages.v1 output facts. User input/uptake is a separate
// adapter boundary; neither accepted operations nor Session unread flags belong here.
export type AgentOutputFact = Readonly<{
  id: string; agentRunId: string; turnId: string; toolCallId: string;
  sourceSequence: number; createdAtMs: number; body: string;
  sessionRefs: readonly string[]; fileRefs: readonly string[];
}>;
export type AgentOutputPage = Readonly<{
  schema: "agent.messages.v1"; agentId: string; sessionId: string;
  messages: readonly AgentOutputFact[]; nextAfterSequence: number | null;
}>;
export type ReferencedSession = AgentSessionReferenceView & Readonly<{ agentId: string }>;

function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("agent_output_response_invalid");
  return value as Record<string, unknown>;
}
function exactKeys(value: Record<string, unknown>, names: string) {
  if (Object.keys(value).sort().join("|") !== names.split("|").sort().join("|")) throw new Error("agent_output_response_invalid");
}
function opaque(value: unknown): value is string {
  return typeof value === "string" && Boolean(value) && value.trim() === value;
}
function refs(value: unknown): value is string[] {
  return Array.isArray(value) && value.length <= 8 && value.every(opaque);
}

export function parseAgentOutputPage(value: unknown, agentId: string, afterSequence: number, sessionId?: string): AgentOutputPage {
  const page = record(value);
  exactKeys(page, "schema|agentId|sessionId|messages|nextAfterSequence");
  if (page.schema !== "agent.messages.v1" || page.agentId !== agentId || !opaque(page.sessionId)
    || (sessionId !== undefined && page.sessionId !== sessionId) || !Array.isArray(page.messages) || page.messages.length > 50
    || !Number.isSafeInteger(afterSequence) || afterSequence < 0) throw new Error("agent_output_response_invalid");
  let previous = afterSequence;
  const ids = new Set<string>();
  for (const value of page.messages) {
    const message = record(value);
    exactKeys(message, "id|agentRunId|turnId|toolCallId|sourceSequence|createdAtMs|body|sessionRefs|fileRefs");
    if (![message.id, message.agentRunId, message.turnId, message.toolCallId].every(opaque)
      || typeof message.sourceSequence !== "number" || !Number.isSafeInteger(message.sourceSequence) || message.sourceSequence <= previous
      || typeof message.createdAtMs !== "number" || !Number.isSafeInteger(message.createdAtMs) || message.createdAtMs < 0 || !Number.isFinite(new Date(message.createdAtMs).getTime())
      || typeof message.body !== "string" || !message.body.trim() || !refs(message.sessionRefs) || !refs(message.fileRefs)
      || ids.has(message.id as string)) throw new Error("agent_output_response_invalid");
    ids.add(message.id as string);
    previous = message.sourceSequence;
  }
  const next = page.nextAfterSequence;
  if (next !== null && (typeof next !== "number" || !Number.isSafeInteger(next) || next <= afterSequence || next < previous)) {
    throw new Error("agent_output_cursor_invalid");
  }
  return structuredClone(page) as AgentOutputPage;
}

export function mergeAgentOutputs(existing: readonly AgentOutputFact[], incoming: readonly AgentOutputFact[]) {
  const byId = new Map(existing.map(message => [message.id, message]));
  const merged = [...existing];
  for (const message of incoming) {
    const previous = byId.get(message.id);
    if (previous) {
      for (const key of ["agentRunId", "turnId", "toolCallId", "sourceSequence", "createdAtMs", "body", "sessionRefs", "fileRefs"] as const) {
        if (JSON.stringify(previous[key]) !== JSON.stringify(message[key])) throw new Error("agent_output_identity_conflict");
      }
      continue;
    }
    if (merged.length && message.sourceSequence <= merged[merged.length - 1].sourceSequence) throw new Error("agent_output_order_invalid");
    byId.set(message.id, message);
    merged.push(message);
  }
  return merged;
}

export function projectAgentOutputs(messages: readonly AgentOutputFact[], authorLabel: string, sessions: ReadonlyMap<string, AgentSessionReferenceView>): readonly Extract<AgentMessageView, { role: "agent" }>[] {
  return messages.map(message => ({
    role: "agent", messageId: message.id, authorLabel, text: message.body,
    // A loaded output page does not prove latest-by-role across the full history.
    isLatest: false, createdAt: new Date(message.createdAtMs).toISOString(),
    sessions: message.sessionRefs.flatMap(id => { const session = sessions.get(id); return session ? [session] : []; }),
  }));
}

export function parseReferencedSession(value: unknown, workspaceId: string, sessionId: string): ReferencedSession {
  const session = record(record(value).session);
  if (session.id !== sessionId || session.workspaceId !== workspaceId || !opaque(session.agentId)
    || typeof session.title !== "string" || !session.title.trim() || session.status !== "active"
    || typeof session.hasActiveAgentRun !== "boolean") throw new Error("agent_session_reference_invalid");
  // hasActiveAgentRun includes queued work. false does not prove completion.
  return { sessionId, title: session.title, agentId: session.agentId, runState: "unknown" };
}
