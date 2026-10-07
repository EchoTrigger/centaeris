import { parseAgentOutputPage, type AgentOutputFact } from "./agentOutputFeed.ts";
import { parsePreparedAgentInputFact, type PreparedAgentInputFact, type PreparedAgentInputBinding } from "./preparedAgentInput.ts";
import type { AgentMessageView, AgentSessionReferenceView } from "./agentChatViewTypes";

export type AgentHistoryItem = Readonly<{ cursor: string }> & (
  | Readonly<{ kind: "input"; input: PreparedAgentInputFact }>
  | Readonly<{ kind: "message"; message: AgentOutputFact }>
);
export type AgentHistoryPage = Readonly<{ schema: "agent.history.v1"; agentId: string; sessionId: string; items: readonly AgentHistoryItem[]; nextCursor: string | null; newestCursor: string | null; hasMore: boolean }>;
function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("agent_history_response_invalid");
  return value as Record<string, unknown>;
}
function exact(value: Record<string, unknown>, fields: string) {
  if (Object.keys(value).sort().join("|") !== fields.split("|").sort().join("|")) throw new Error("agent_history_response_invalid");
}
function opaque(value: unknown): value is string {
  return typeof value === "string" && Boolean(value) && value.trim() === value;
}
function cursor(value: unknown): value is string {
  return opaque(value) && value.length <= 512;
}
export function agentHistoryPath(agentId: string, afterCursor: string | null, limit = 50, direction: "older" | "newer" = "older") {
  if (!opaque(agentId) || (afterCursor !== null && !cursor(afterCursor)) || !Number.isSafeInteger(limit) || limit < 1 || limit > 100) throw new Error("agent_history_request_invalid");
  const query = new URLSearchParams({ limit: String(limit) });
  if (afterCursor !== null) query.set(direction === "older" ? "beforeCursor" : "afterCursor", afterCursor);
  return `/api/agents/${encodeURIComponent(agentId)}/history?${query}`;
}

export function parseAgentHistoryPage(value: unknown, agentId: string, afterCursor: string | null, sessionId?: string, limit = 50, direction: "older" | "newer" = "older"): AgentHistoryPage {
  agentHistoryPath(agentId, afterCursor, limit);
  const page = record(value);
  exact(page, "schema|agentId|sessionId|items|nextCursor|newestCursor|hasMore");
  if (page.schema !== "agent.history.v1" || page.agentId !== agentId || !opaque(page.sessionId) || (sessionId !== undefined && page.sessionId !== sessionId)
    || !Array.isArray(page.items) || page.items.length > limit || typeof page.hasMore !== "boolean" || (page.nextCursor !== null && !cursor(page.nextCursor)) || (page.newestCursor !== null && !cursor(page.newestCursor)) || (page.items.length && page.newestCursor === null)) throw new Error("agent_history_response_invalid");
  const cursors = new Set<string>();
  const identities = new Set<string>();
  const items = page.items.map(value => {
    const row = record(value);
    exact(row, row.kind === "input" ? "cursor|kind|input" : "cursor|kind|message");
    if (!cursor(row.cursor) || row.cursor === afterCursor || cursors.has(row.cursor)) throw new Error("agent_history_cursor_invalid");
    cursors.add(row.cursor);
    let item: AgentHistoryItem;
    if (row.kind === "input") {
      item = { cursor: row.cursor, kind: "input", input: parsePreparedAgentInputFact(row.input) };
    } else if (row.kind === "message") {
      // Reuse the exact committed-output validator, without merging its sequence
      // domain into inputs or deriving an order from timestamps.
      const message = parseAgentOutputPage({ schema: "agent.messages.v1", agentId, sessionId: page.sessionId, messages: [row.message], nextAfterSequence: null }, agentId, 0).messages[0];
      item = { cursor: row.cursor, kind: "message", message };
    } else throw new Error("agent_history_kind_invalid");
    const key = historyIdentity(item);
    if (identities.has(key)) throw new Error("agent_history_identity_conflict");
    identities.add(key);
    return item;
  });
  const next = page.nextCursor;
  if ((items.length && (next === null || next === afterCursor)) || (page.hasMore && (next === null || next === afterCursor))
    || (typeof next === "string" && cursors.has(next) && (direction === "older" ? items[0] : items.at(-1))?.cursor !== next)) throw new Error("agent_history_cursor_invalid");
  return { schema: "agent.history.v1", agentId, sessionId: page.sessionId, items, nextCursor: next as string | null, newestCursor: page.newestCursor as string | null, hasMore: page.hasMore };
}
function historyIdentity(item: AgentHistoryItem) {
  return item.kind === "input" ? `input:${item.input.inputId}` : `message:${item.message.id}`;
}
export function mergeAgentHistory(existing: readonly AgentHistoryItem[], incoming: readonly AgentHistoryItem[]): readonly AgentHistoryItem[] {
  const merged = [...existing];
  const indices = new Map(existing.map((item, index) => [historyIdentity(item), index]));
  const cursors = new Set(existing.map(item => item.cursor));
  let previousIndex = -1;
  for (const item of incoming) {
    const key = historyIdentity(item);
    const index = indices.get(key);
    if (index !== undefined) {
      const prior = merged[index];
      const sameFact = prior.kind === "input" && item.kind === "input"
        ? prior.input.sequence === item.input.sequence && prior.input.createdAtMs === item.input.createdAtMs && prior.input.body === item.input.body && JSON.stringify(prior.input.attachments) === JSON.stringify(item.input.attachments)
          && (prior.input.read === null || JSON.stringify(prior.input.read) === JSON.stringify(item.input.read))
        : JSON.stringify(prior) === JSON.stringify(item);
      if (index <= previousIndex || prior.cursor !== item.cursor || !sameFact) throw new Error("agent_history_identity_conflict");
      merged[index] = item; previousIndex = index;
    } else {
      if (cursors.has(item.cursor)) throw new Error("agent_history_cursor_invalid");
      const nextIndex = merged.length;
      indices.set(key, nextIndex); cursors.add(item.cursor); merged.push(item); previousIndex = nextIndex;
    }
  }
  return merged;
}
export function prependAgentHistory(existing: readonly AgentHistoryItem[], incoming: readonly AgentHistoryItem[]): readonly AgentHistoryItem[] {
  const identities = new Set(existing.map(historyIdentity));
  if (incoming.some(item => identities.has(historyIdentity(item)))) throw new Error("agent_history_identity_conflict");
  return mergeAgentHistory(incoming, existing);
}
export function projectAgentHistory(items: readonly AgentHistoryItem[], userLabel: string, agentLabel: string, sessions: ReadonlyMap<string, AgentSessionReferenceView>, completeTail: boolean, binding?: PreparedAgentInputBinding): readonly AgentMessageView[] {
  let lastInput = -1; let lastMessage = -1;
  if (completeTail) items.forEach((item, index) => { if (item.kind === "input") lastInput = index; else lastMessage = index; });
  return items.map((item, index) => item.kind === "input" ? {
    role: "user", messageId: item.input.inputId, authorLabel: userLabel, text: item.input.body, createdAt: new Date(item.input.createdAtMs).toISOString(), isLatest: index === lastInput,
    ...(binding && item.input.attachments.length ? { attachments: item.input.attachments, attachmentBinding: { agentId: binding.agentId, inputId: item.input.inputId } } : {}),
    ...(item.input.read ? { loopInputFact: { kind: "loopInput" as const, messageId: item.input.inputId, receivedAt: new Date(item.input.read.createdAtMs).toISOString() } } : {}),
  } : {
    role: "agent", messageId: item.message.id, authorLabel: agentLabel, text: item.message.body, createdAt: new Date(item.message.createdAtMs).toISOString(), isLatest: index === lastMessage,
    sessions: item.message.sessionRefs.flatMap(id => { const session = sessions.get(id); return session ? [session] : []; }),
    ...(binding && item.message.fileRefs.length ? { fileBinding: { ...binding, agentRunId: item.message.agentRunId, inputRefs: item.message.fileRefs } } : {}),
  });
}
