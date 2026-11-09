import type {
  SessionStreamEvent,
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

export function isTerminalSessionEvent(type: string) {
  return TERMINAL_EVENT_TYPES.has(type);
}
