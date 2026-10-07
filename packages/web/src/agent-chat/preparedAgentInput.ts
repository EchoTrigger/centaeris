import type { AgentMessageView } from "./agentChatViewTypes";

// Exact hosted input carrier/text validation, reused by authoritative history.
// Read identifies the committed main request that took up this input.
export type PreparedAgentInputBinding = Readonly<{ agentId: string; sessionId: string }>;
export type AgentInputAttachment = Readonly<{ inputRef: string; displayName: string; contentType: string }>;
export type PreparedAgentInputSubmission = Readonly<{ schema: "agent.input.submit.v1"; inputId: string; body: string; attachmentRefs: readonly string[] }>;
export type AgentInputRead = Readonly<{ agentRunId: string; eventId: string; requestId: string; createdAtMs: number }>;
export type PreparedAgentInputFact = Readonly<{ inputId: string; sequence: number; createdAtMs: number; body: string; attachments: readonly AgentInputAttachment[]; read: AgentInputRead | null }>;
export type PreparedAgentInputAcceptance = PreparedAgentInputBinding & Readonly<{ schema: "agent.input.accepted.v1"; input: PreparedAgentInputFact }>;
export type PreparedAgentInputPage = PreparedAgentInputBinding & Readonly<{ schema: "agent.inputs.v1"; inputs: readonly PreparedAgentInputFact[]; nextAfterSequence: number | null }>;

function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("agent_input_response_invalid");
  return value as Record<string, unknown>;
}
function exactKeys(value: Record<string, unknown>, names: string) {
  if (Object.keys(value).sort().join("|") !== names.split("|").sort().join("|")) throw new Error("agent_input_response_invalid");
}
function inputId(value: unknown): string {
  if (typeof value !== "string" || !value.length || value.length > 64 || /[^A-Za-z0-9_.:-]/u.test(value)) throw new Error("agent_input_id_invalid");
  return value;
}
function inputBody(value: unknown, hasAttachments = false): string {
  // Match the Python draft's strip() rejection without changing the stored text.
  if (typeof value !== "string" || (!hasAttachments && /^[\u0009-\u000d\u001c-\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]*$/u.test(value)) || value.includes("\0")) throw new Error("agent_input_body_invalid");
  // TextEncoder replaces lone UTF-16 surrogates; reject them before UTF-8 counting.
  for (let index = 0; index < value.length; index++) {
    const code = value.charCodeAt(index);
    if (code >= 0xd800 && code <= 0xdbff) {
      const low = value.charCodeAt(++index);
      if (!(low >= 0xdc00 && low <= 0xdfff)) throw new Error("agent_input_body_invalid");
    } else if (code >= 0xdc00 && code <= 0xdfff) throw new Error("agent_input_body_invalid");
  }
  if (new TextEncoder().encode(value).length > 65536) throw new Error("agent_input_body_invalid");
  return value;
}
function positiveSequence(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0;
}
function validateBinding(response: Record<string, unknown>, binding: PreparedAgentInputBinding) {
  if (![binding.agentId, binding.sessionId].every(id => typeof id === "string" && Boolean(id) && id.trim() === id)
    || response.agentId !== binding.agentId || response.sessionId !== binding.sessionId) throw new Error("agent_input_binding_invalid");
}
function timestamp(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 && Number.isFinite(new Date(value).getTime());
}
export function parseAgentAttachmentRefs(value: unknown): readonly string[] {
  if (!Array.isArray(value) || value.length > 50 || value.some(ref => typeof ref !== "string" || !ref || ref.trim() !== ref)
    || value.some((ref, index) => index > 0 && value[index - 1] >= ref)) throw new Error("agent_input_attachments_invalid");
  return Object.freeze([...value]);
}
export function parseAgentInputAttachments(value: unknown): readonly AgentInputAttachment[] {
  if (!Array.isArray(value)) throw new Error("agent_input_attachments_invalid");
  const attachments = value.map(item => {
    const row = record(item); exactKeys(row, "inputRef|displayName|contentType");
    if (![row.inputRef, row.displayName, row.contentType].every(field => typeof field === "string" && Boolean(field) && field.trim() === field)) throw new Error("agent_input_attachments_invalid");
    return Object.freeze({ inputRef: row.inputRef as string, displayName: row.displayName as string, contentType: row.contentType as string });
  });
  parseAgentAttachmentRefs(attachments.map(item => item.inputRef));
  return Object.freeze(attachments);
}
function parseRead(value: unknown): AgentInputRead | null {
  if (value === null) return null;
  const row = record(value);
  exactKeys(row, "agentRunId|eventId|requestId|createdAtMs");
  if (![row.agentRunId, row.eventId, row.requestId].every(id => typeof id === "string" && Boolean(id) && id.trim() === id)
    || !timestamp(row.createdAtMs)) throw new Error("agent_input_read_invalid");
  return { agentRunId: row.agentRunId as string, eventId: row.eventId as string, requestId: row.requestId as string, createdAtMs: row.createdAtMs };
}
export function parsePreparedAgentInputFact(value: unknown): PreparedAgentInputFact {
  const row = record(value);
  exactKeys(row, "inputId|sequence|createdAtMs|body|attachments|read");
  if (!positiveSequence(row.sequence) || !timestamp(row.createdAtMs)) throw new Error("agent_input_response_invalid");
  const attachments = parseAgentInputAttachments(row.attachments);
  return { inputId: inputId(row.inputId), sequence: row.sequence, createdAtMs: row.createdAtMs, body: inputBody(row.body, attachments.length > 0), attachments, read: parseRead(row.read) };
}

export function prepareAgentInputSubmission(id: string, body: string, attachmentRefs: readonly string[] = []): PreparedAgentInputSubmission {
  // The caller retains this identity and exact text across an uncertain retry.
  const refs = parseAgentAttachmentRefs(attachmentRefs);
  return Object.freeze({ schema: "agent.input.submit.v1", inputId: inputId(id), body: inputBody(body, refs.length > 0), attachmentRefs: refs });
}

export function parsePreparedAgentInputAcceptance(value: unknown, binding: PreparedAgentInputBinding, submission: PreparedAgentInputSubmission): PreparedAgentInputAcceptance {
  const response = record(value);
  exactKeys(response, "schema|agentId|sessionId|input");
  validateBinding(response, binding);
  if (response.schema !== "agent.input.accepted.v1" || submission.schema !== "agent.input.submit.v1") throw new Error("agent_input_response_invalid");
  const input = parsePreparedAgentInputFact(response.input);
  if (input.inputId !== inputId(submission.inputId) || input.body !== inputBody(submission.body, submission.attachmentRefs.length > 0)
    || JSON.stringify(input.attachments.map(item => item.inputRef)) !== JSON.stringify(parseAgentAttachmentRefs(submission.attachmentRefs))) throw new Error("agent_input_acceptance_mismatch");
  return { schema: "agent.input.accepted.v1", agentId: binding.agentId, sessionId: binding.sessionId, input };
}

export function parsePreparedAgentInputPage(value: unknown, binding: PreparedAgentInputBinding, afterSequence: number, limit = 50): PreparedAgentInputPage {
  const response = record(value);
  exactKeys(response, "schema|agentId|sessionId|inputs|nextAfterSequence");
  validateBinding(response, binding);
  if (response.schema !== "agent.inputs.v1" || !Array.isArray(response.inputs) || !Number.isSafeInteger(afterSequence) || afterSequence < 0
    || !Number.isSafeInteger(limit) || limit < 1 || limit > 100 || response.inputs.length > limit) throw new Error("agent_input_response_invalid");
  const inputs = response.inputs.map(parsePreparedAgentInputFact);
  let previous = afterSequence;
  const ids = new Set<string>();
  for (const input of inputs) {
    if (input.sequence <= previous || ids.has(input.inputId)) throw new Error("agent_input_order_invalid");
    previous = input.sequence;
    ids.add(input.inputId);
  }
  const next = response.nextAfterSequence;
  if (next !== null && (!positiveSequence(next) || next <= afterSequence || inputs.length !== limit || next !== previous)) throw new Error("agent_input_cursor_invalid");
  return { schema: "agent.inputs.v1", agentId: binding.agentId, sessionId: binding.sessionId, inputs, nextAfterSequence: next };
}

export function projectPreparedAgentInputs(inputs: readonly PreparedAgentInputFact[], authorLabel: string): readonly Extract<AgentMessageView, { role: "user" }>[] {
  return inputs.map(input => ({
    role: "user", messageId: input.inputId, authorLabel, text: input.body, createdAt: new Date(input.createdAtMs).toISOString(),
    // A loaded input page proves neither latest across history nor actual uptake.
    isLatest: false,
  }));
}
