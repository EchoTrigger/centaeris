export type SessionSubagentStatus = "queued" | "leased" | "running" | "succeeded" | "failed" | "dead_lettered" | "cancelled";

export type SessionSubagent = Readonly<{
  runtimeJobId: string;
  callId: string;
  childSessionId: string;
  parentAgentRunId: string;
  parentTurnId: string;
  description: string;
  prompt: string;
  status: SessionSubagentStatus;
  createdAtMs: number;
  updatedAtMs: number;
}>;

export type SessionSubagentMessage = Readonly<{
  messageId: string;
  role: "user" | "assistant" | "tool" | "system";
  content: string;
  createdAtMs: number;
  reasoningContent: string | null;
  toolCalls: readonly Readonly<{ id: string; name: string; argsJson: string }>[];
  toolCallId: string | null;
  toolName: string | null;
  resultState: string | null;
}>;

const statuses = new Set(["queued", "leased", "running", "succeeded", "failed", "dead_lettered", "cancelled"]);
const agentKeys = ["runtimeJobId", "callId", "childSessionId", "parentAgentRunId", "parentTurnId", "description", "prompt", "status", "createdAtMs", "updatedAtMs"];

function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function exact(value: Record<string, unknown>, keys: readonly string[]) {
  return Object.keys(value).length === keys.length && keys.every(key => Object.hasOwn(value, key));
}

function agent(value: unknown): value is SessionSubagent {
  return record(value) && exact(value, agentKeys)
    && agentKeys.slice(0, 8).every(key => typeof value[key] === "string")
    && agentKeys.slice(0, 5).every(key => (value[key] as string).length > 0)
    && statuses.has(value.status as string)
    && Number.isSafeInteger(value.createdAtMs) && Number.isSafeInteger(value.updatedAtMs);
}

export function parseSessionSubagents(value: unknown, sessionId: string): readonly SessionSubagent[] {
  if (!record(value) || !exact(value, ["schema", "sessionId", "agents"])
    || value.schema !== "workspace.session.subagents.v1" || value.sessionId !== sessionId
    || !Array.isArray(value.agents) || !value.agents.every(agent)
    || new Set(value.agents.map(item => item.runtimeJobId)).size !== value.agents.length
    || new Set(value.agents.map(item => item.callId)).size !== value.agents.length) {
    throw new Error("invalid session subagents");
  }
  return value.agents;
}

export function parseSessionSubagent(value: unknown, sessionId: string, runtimeJobId: string) {
  const messageKeys = ["messageId", "role", "content", "createdAtMs", "reasoningContent", "toolCalls", "toolCallId", "toolName", "resultState"];
  if (!record(value) || !exact(value, ["schema", "sessionId", "agent", "messages"])
    || value.schema !== "workspace.session.subagent.v1" || value.sessionId !== sessionId
    || !agent(value.agent) || value.agent.runtimeJobId !== runtimeJobId
    || !Array.isArray(value.messages) || !value.messages.every(message => record(message)
      && exact(message, messageKeys) && typeof message.messageId === "string"
      && ["user", "assistant", "tool", "system"].includes(message.role as string)
      && typeof message.content === "string" && Number.isSafeInteger(message.createdAtMs)
      && ["reasoningContent", "toolCallId", "toolName", "resultState"].every(key => message[key] === null || typeof message[key] === "string")
      && Array.isArray(message.toolCalls) && message.toolCalls.every(call => record(call)
        && exact(call, ["id", "name", "argsJson"]) && [call.id, call.name, call.argsJson].every(item => typeof item === "string")))) {
    throw new Error("invalid session subagent");
  }
  return { agent: value.agent, messages: value.messages as readonly SessionSubagentMessage[] };
}

export function sortSessionSubagents(agents: readonly SessionSubagent[]) {
  const priority = (item: SessionSubagent) => item.status === "running" ? 0 : item.status === "queued" || item.status === "leased" ? 1 : 2;
  return [...agents].sort((a, b) => priority(a) - priority(b) || a.createdAtMs - b.createdAtMs || a.runtimeJobId.localeCompare(b.runtimeJobId));
}

export function matchedSubagentWait(input: unknown, agents: readonly SessionSubagent[]): SessionSubagent | undefined {
  if (!record(input) || !record(input.output_ref)) return undefined;
  const ref = input.output_ref;
  if (!exact(ref, ["schema", "kind", "runtime_job_id", "child_session_id", "result_ref"])
    || ref.schema !== "task_output_ref_v1" || ref.kind !== "agent"
    || typeof ref.result_ref !== "string" || !ref.result_ref) return undefined;
  return agents.find(item => item.runtimeJobId === ref.runtime_job_id && item.childSessionId === ref.child_session_id);
}
