import type {
  SessionStreamEvent,
  StreamEntry,
  StreamItem,
  UnknownRecord,
} from "./streamTypes.ts";

const VISIBLE_EVENT_TYPES = new Set([
  "agent_run_started", "user_message", "turn_supplement", "assistant_message", "tool_call",
  "tool_result", "phase_event", "external_evidence_ref", "citation_recorded",
  "artifact_published", "compaction", "tombstone", "agent_run_completed", "agent_run_failed",
  "agent_run_interrupted", "reasoning_block",
]);
const TERMINAL_EVENT_TYPES = new Set([
  "agent_run_completed",
  "agent_run_failed",
  "agent_run_interrupted",
]);
const STREAM_ITEM_FIELDS = {
  committed: ["event", "kind", "agentRunId", "schema", "sourceSequence"],
  live: ["afterSequence", "kind", "messageId", "revision", "agentRunId", "schema", "text", "turnId"],
};

export function isRecord(value: unknown): value is UnknownRecord {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

export function isInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value);
}

export function hasExactFields(value: UnknownRecord, fields: readonly string[]) {
  return Object.keys(value).sort().join("|") === [...fields].sort().join("|");
}

export function requireObject(value: unknown, name: string): asserts value is UnknownRecord {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${name} must be an object`);
}

export function requireString(
  value: unknown,
  name: string,
  allowEmpty = false,
): asserts value is string {
  if (typeof value !== "string" || (!allowEmpty && !value.trim())) throw new Error(`${name} must be a string`);
}

export function validateSessionEvent(
  event: unknown,
  identity: Readonly<{ sessionId?: string; agentRunId?: string }> = {},
): SessionStreamEvent {
  requireObject(event, "session event");
  const required = ["sessionId", "createdAtMs", "eventId", "eventVersion", "payload", "schemaVersion", "sequence", "type"];
  const allowed = new Set([...required, "agentRunId", "turnId"]);
  if (required.some((field) => !(field in event)) || Object.keys(event).some((field) => !allowed.has(field))) {
    throw new Error("session event fields mismatch");
  }
  if (event.schemaVersion !== "session.event.v1" || event.eventVersion !== 1 || typeof event.type !== "string" || !VISIBLE_EVENT_TYPES.has(event.type)) {
    throw new Error("session event schema or type is unsupported");
  }
  for (const field of ["eventId", "sessionId"]) requireString(event[field], `session event ${field}`);
  if (!isInteger(event.createdAtMs) || event.createdAtMs < 0) throw new Error("session event createdAtMs is invalid");
  if (!isInteger(event.sequence) || event.sequence <= 0) throw new Error("session event sequence is invalid");
  requireObject(event.payload, "session event payload");
  if (identity.sessionId && event.sessionId !== identity.sessionId) throw new Error("session event session binding mismatch");
  if (identity.agentRunId && event.agentRunId !== identity.agentRunId) throw new Error("Session event AgentRun binding mismatch");
  return event as SessionStreamEvent;
}

export function validateLiveReasoning(value: unknown) {
  if (value === undefined || value === null) return value;
  requireObject(value, "live reasoning");
  if (!hasExactFields(value, ["blockId", "requestId", "text"])) throw new Error("live reasoning fields mismatch");
  requireString(value.requestId, "live reasoning requestId");
  requireString(value.text, "live reasoning text", true);
  if (value.blockId !== `reasoning:${value.requestId}`) throw new Error("live reasoning identity mismatch");
  return { blockId: value.blockId as string, requestId: value.requestId, text: value.text };
}

function validateStreamItem(item: unknown, agentRunId: string): StreamItem {
  requireObject(item, "session stream item");
  if (item.kind !== "committed" && item.kind !== "live") {
    throw new Error("session stream item fields or binding are invalid");
  }
  const fields = [...STREAM_ITEM_FIELDS[item.kind], ...(item.kind === "live" && Object.hasOwn(item, "reasoning") ? ["reasoning"] : [])];
  if (!hasExactFields(item, fields) || item.schema !== "session.stream.item.v1" || item.agentRunId !== agentRunId) {
    throw new Error("session stream item fields or binding are invalid");
  }
  if (item.kind === "committed") {
    if (!isInteger(item.sourceSequence) || item.sourceSequence <= 0) throw new Error("sourceSequence is invalid");
    const event = validateSessionEvent(item.event, { agentRunId });
    if (event.sequence !== item.sourceSequence) throw new Error("stream session event sequence binding mismatch");
  } else {
    if (!isInteger(item.afterSequence) || item.afterSequence < 0 || !isInteger(item.revision) || item.revision <= 0) throw new Error("live stream sequence is invalid");
    for (const field of ["messageId", "turnId"]) requireString(item[field], `live ${field}`);
    requireString(item.text, "live text", true);
    validateLiveReasoning(item.reasoning);
  }
  return item as StreamItem;
}

export function isTerminalSessionEvent(type: string) {
  return TERMINAL_EVENT_TYPES.has(type);
}

export function readSseBlock(block: string, agentRunId: string): StreamEntry | null {
  let cursor = null;
  const dataLines: string[] = [];
  for (const line of block.split(/\r?\n/)) {
    if (line.startsWith("id:")) cursor = line.slice(3).trim();
    else if (line.startsWith("data:")) dataLines.push(line.slice(5).trimStart());
  }
  if (!dataLines.length) return null;
  return { cursor, item: validateStreamItem(JSON.parse(dataLines.join("\n")), agentRunId) };
}

function throwIfAborted(signal?: AbortSignal) {
  if (signal?.aborted) throw new DOMException("Aborted", "AbortError");
}

export async function readSse(
  response: Response,
  agentRunId: string,
  onItem: (entry: StreamEntry) => void | Promise<void>,
  { signal }: { signal?: AbortSignal } = {},
) {
  const reader = response.body?.getReader();
  if (!reader) throw new Error("stream body is unavailable");
  const cancel = () => { void reader.cancel().catch(() => {}); };
  signal?.addEventListener("abort", cancel, { once: true });
  const decoder = new TextDecoder();
  let buffer = "";
  let terminal = false;
  const deliver = async (block: string) => {
    throwIfAborted(signal);
    const entry = readSseBlock(block, agentRunId);
    if (!entry) return;
    if (terminal) throw new Error("session stream emitted an item after terminal");
    await onItem(entry);
    terminal = entry.item.kind === "committed" && isTerminalSessionEvent(entry.item.event.type);
  };
  try {
    while (true) {
      throwIfAborted(signal);
      const { value, done } = await reader.read();
      throwIfAborted(signal);
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      const blocks = buffer.split(/\r?\n\r?\n/);
      buffer = blocks.pop() || "";
      for (const block of blocks) await deliver(block);
    }
    buffer += decoder.decode();
    if (buffer.trim()) await deliver(buffer);
    return terminal;
  } finally {
    signal?.removeEventListener("abort", cancel);
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
