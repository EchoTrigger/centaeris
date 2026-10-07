import { validateCitationSnapshot } from "./citationSnapshot.ts";
import type {
  SessionStreamEvent,
  StreamEntry,
  UnknownRecord,
} from "./streamTypes.ts";
import {
  hasExactFields,
  isInteger,
  isRecord,
  isTerminalSessionEvent,
  requireObject,
  requireString,
  validateLiveReasoning,
  validateSessionEvent,
} from "./sessionStreamProtocol.ts";

export { readSse, readSseBlock } from "./sessionStreamProtocol.ts";

// Frontend view data, separate from model-visible messages and wire schemas.
export type ReasoningBlockView = {
  id: string;
  turnId?: string;
  sequence: number;
  text: string;
  status: "streaming" | "done" | "interrupted";
};

export type AgentRunStatus =
  | "queued"
  | "running"
  | "completed"
  | "failed"
  | "cancelled";

export type AgentRunIdentity = {
  sessionId?: string;
  workspaceId?: string;
  agentRunId?: string;
};

export type StoredSessionEvent = {
  sequence: number;
  event: SessionStreamEvent;
};

export type LiveAssistant = {
  reasoning?: { blockId: string; requestId: string; text: string } | null;
  afterSequence: number;
  messageId: string;
  revision: number;
  text: string;
  turnId: string;
};

export type ArtifactLink = UnknownRecord & {
  artifactRef: string;
  filename: string;
  downloadUrl: string;
};

export type ChatMessage = UnknownRecord & {
  messageId: string;
  turnId?: string;
  sequence: number;
  role: string;
  phase: string;
  status: unknown;
  text: string;
  createdAtMs?: number;
  attachments: unknown[];
  artifacts: ArtifactLink[];
};

export type ToolActivity = UnknownRecord & {
  activityId: string;
  callId: string;
  toolName: string;
  turnId?: string;
  sequence: number;
  status: string;
  childSessionId: string | null | undefined;
  call: UnknownRecord;
  result: UnknownRecord | null;
  waitResult: UnknownRecord | null;
};

export type Citation = UnknownRecord & {
  citationId: string;
  sourceUrl: string;
};

type RawAgentRun = UnknownRecord & {
  id: string;
  status: AgentRunStatus;
  model: unknown;
  createdAt: string;
  startedAt: string | null;
  completedAt: string | null;
  events: StoredSessionEvent[];
  live: LiveAssistant | null;
  streamCursor: string;
  citations: Citation[];
  citationSequence: number;
  sessionId?: string;
  workspaceId?: string;
};

type AgentRunViewState = RawAgentRun & {
  reasoningBlocks: ReasoningBlockView[];
  messages: ChatMessage[];
  activities: ToolActivity[];
  citations: Citation[];
  artifacts: ArtifactLink[];
  agentWaits: Record<string, string>;
  overlayBarrierByTurnId: Record<string, number>;
  startedAtMs: number;
  phaseKey: string;
  phaseStartedAtMs: number;
  finishedAtMs: number | null;
  connection: string;
  projectionError: string | null;
  failureReason?: unknown;
  interruptionReason?: unknown;
};

export type ProjectedAgentRun = AgentRunViewState & {
  eventIds: string[];
  lastSourceSequence: number;
};

export type TranscriptProjectionWork = {
  committedEventVisits: number;
  liveOverlayApplications: number;
};

export const createTranscriptProjectionWork = (): TranscriptProjectionWork => ({
  committedEventVisits: 0,
  liveOverlayApplications: 0,
});

export type HistoryPage = UnknownRecord & {
  schema: typeof HISTORY_PAGE_SCHEMA;
  session: UnknownRecord & { id: string; workspaceId: string };
  agentRuns: ProjectedAgentRun[];
  nextCursor: string | null;
  hasMore: boolean;
};

const AGENT_RUN_STATUSES = new Set(["queued", "running", "completed", "failed", "cancelled"]);
export const HISTORY_PAGE_SCHEMA = "session.history.page.v1";

export function validateOperation(operation: unknown, callId: string): UnknownRecord {
  requireObject(operation, "tool operation");
  for (const field of ["callId", "toolName", "status", "resultState"]) {
    requireString(operation[field], `tool operation ${field}`);
  }
  requireString(operation.toolName, "tool operation toolName");
  requireString(operation.resultState, "tool operation resultState");
  if (operation.callId !== callId) throw new Error("tool operation call binding mismatch");
  if (Object.hasOwn(operation, "title")) throw new Error("tool operation title is removed");
  if (operation.toolName === "bash") {
    if (operation.kind !== "command") throw new Error("bash tool operation kind must be command");
  } else if (Object.hasOwn(operation, "kind")) {
    throw new Error("non-bash tool operation kind is removed");
  }
  if (["write", "edit"].includes(operation.toolName)) {
    const failed = ["failed", "denied", "aborted"].includes(operation.resultState);
    if (failed && Object.hasOwn(operation, "diffPreview")) throw new Error("failed mutation operation must not carry diffPreview");
    if (!failed) requireString(operation.diffPreview, "successful mutation diffPreview");
  }
  return { ...operation };
}

export function validateActivity(activity: unknown): ToolActivity {
  requireObject(activity, "activity");
  for (const field of ["activityId", "callId", "turnId", "toolName", "status"]) {
    requireString(activity[field], `activity ${field}`);
  }
  requireObject(activity.call, "activity tool call");
  if (activity.call.callId !== activity.callId || activity.call.toolName !== activity.toolName) {
    throw new Error("activity tool call identity mismatch");
  }
  if (activity.result !== null) {
    requireObject(activity.result, "activity tool result");
    if (activity.result.callId !== activity.callId || activity.result.toolName !== activity.toolName) {
      throw new Error("activity tool result identity mismatch");
    }
  }
  return {
    ...activity,
    call: { ...activity.call },
    result: activity.result ? { ...activity.result } : null,
  } as ToolActivity;
}

function upsertBy<T>(items: readonly T[], key: keyof T, value: T) {
  const index = items.findIndex((item) => item[key] === value[key]);
  return index < 0 ? [...items, value] : items.map((item, itemIndex) => itemIndex === index ? value : item);
}

function artifactLink(payload: UnknownRecord): ArtifactLink {
  requireString(payload.artifactRef, "artifactRef");
  requireString(payload.filename, "artifact filename");
  const artifactId = payload.artifactRef.startsWith("artifact:") ? payload.artifactRef.slice(9) : "";
  if (!artifactId) throw new Error("artifactRef is invalid");
  return { artifactRef: payload.artifactRef, filename: payload.filename, downloadUrl: `/api/artifacts/${artifactId}/download` };
}

function commitOverlayBarrier(
  view: AgentRunViewState,
  event: SessionStreamEvent,
  sequence: number,
): AgentRunViewState {
  requireString(event.turnId, `${event.type} turnId`);
  const previousBarrier = Object.hasOwn(view.overlayBarrierByTurnId, event.turnId)
    ? view.overlayBarrierByTurnId[event.turnId]
    : 0;
  const barrier = Math.max(previousBarrier, sequence);
  const liveMessageId = view.live?.messageId;
  const supersedesLive = view.live?.turnId === event.turnId && view.live.afterSequence < barrier;
  return {
    ...view,
    overlayBarrierByTurnId: barrier === previousBarrier
      ? view.overlayBarrierByTurnId
      : { ...view.overlayBarrierByTurnId, [event.turnId]: barrier },
    messages: supersedesLive
      ? view.messages.filter((message) => message.messageId !== liveMessageId)
      : view.messages,
    live: supersedesLive ? null : view.live,
  };
}

function applySessionEvent(
  view: AgentRunViewState,
  event: SessionStreamEvent,
  sequence: number,
): AgentRunViewState {
  const payload = event.payload;
  if (event.type === "reasoning_block") {
    if (!hasExactFields(payload, ["blockId", "requestId", "text", "status"])) throw new Error("reasoning block fields mismatch");
    requireString(event.turnId, "reasoning turnId");
    requireString(payload.requestId, "reasoning requestId");
    requireString(payload.text, "reasoning text");
    if (payload.blockId !== `reasoning:${payload.requestId}` || (payload.status !== "done" && payload.status !== "interrupted")) throw new Error("reasoning block identity or status is invalid");
    if (view.reasoningBlocks.some((block) => block.id === payload.blockId)) throw new Error("reasoning block is already sealed");
    return { ...view, reasoningBlocks: [...view.reasoningBlocks, {
      id: payload.blockId as string, turnId: event.turnId, sequence, text: payload.text, status: payload.status,
    }] };
  }
  if (event.type === "agent_run_started") {
    return { ...view, startedAtMs: event.createdAtMs };
  }
  if (event.type === "user_message") {
    requireString(payload.messageId, "user messageId");
    requireString(payload.text, "user text");
    if (!Array.isArray(payload.attachments)) throw new Error("user attachments are invalid");
    return {
      ...view,
      messages: upsertBy(view.messages, "messageId", {
        messageId: payload.messageId, turnId: event.turnId, sequence, role: "user", phase: "user",
        status: "done", text: payload.text, createdAtMs: event.createdAtMs,
        attachments: payload.attachments.map((item) => ({ ...item })), artifacts: [],
      }),
    };
  }
  if (event.type === "turn_supplement") {
    requireString(payload.messageId, "supplement messageId");
    requireString(payload.message, "supplement message");
    return {
      ...view,
      messages: upsertBy(view.messages, "messageId", {
        messageId: payload.messageId, turnId: event.turnId, sequence, role: "user", phase: "user",
        status: "done", text: payload.message, createdAtMs: event.createdAtMs, attachments: [], artifacts: [],
      }),
    };
  }
  if (event.type === "phase_event") {
    requireString(payload.message, "phase message");
    const messageId = `message:${event.turnId}:phase:${event.eventId}`;
    const next = commitOverlayBarrier(view, event, sequence);
    return {
      ...next,
      messages: upsertBy(next.messages, "messageId", {
        messageId, turnId: event.turnId, sequence, role: "assistant", phase: "stage",
        status: "done", text: payload.message, createdAtMs: event.createdAtMs, attachments: [], artifacts: [],
      }),
    };
  }
  if (event.type === "compaction") {
    for (const field of ["compactionId", "summaryMessageId", "summaryMarkdown", "createdReason"]) {
      requireString(payload[field], `compaction ${field}`);
    }
    if (payload.firstKeptMessageId !== null) {
      requireString(payload.firstKeptMessageId, "compaction firstKeptMessageId");
    }
    return {
      ...view,
      messages: upsertBy(view.messages, "messageId", {
        messageId: `message:${event.turnId}:compaction:${event.eventId}`,
        turnId: event.turnId,
        sequence,
        role: "assistant",
        phase: "compaction",
        status: "done",
        text: "Compacted conversation",
        createdAtMs: event.createdAtMs,
        attachments: [],
        artifacts: [],
      }),
    };
  }
  if (event.type === "tool_call") {
    for (const field of ["callId", "toolName", "toolContractDigest", "providerId", "displayTarget"]) requireString(payload[field], `tool_call ${field}`);
    requireString(payload.callId, "tool_call callId");
    requireString(payload.toolName, "tool_call toolName");
    requireString(payload.toolContractDigest, "tool_call toolContractDigest");
    if (!/^sha256:[0-9a-f]{64}$/.test(payload.toolContractDigest)) throw new Error("tool_call toolContractDigest is invalid");
    const normalizedInput = payload.normalizedInput;
    requireObject(normalizedInput, "tool_call normalizedInput");
    const outputRef = payload.toolName === "task_output" ? normalizedInput.output_ref : null;
    if (isRecord(outputRef) && outputRef.kind === "agent" && typeof outputRef.child_session_id === "string") {
      const agent = view.activities.find((item) => item.toolName === "agent" && item.childSessionId === outputRef.child_session_id);
      if (!agent) throw new Error("task_output has no matching Agent activity");
      return {
        ...view,
        agentWaits: { ...view.agentWaits, [payload.callId]: agent.activityId },
      };
    }
    return {
      ...view,
      activities: upsertBy(view.activities, "activityId", {
        activityId: `activity:${payload.callId}`, callId: payload.callId, toolName: payload.toolName,
        turnId: event.turnId, sequence, status: "running", childSessionId: null,
        call: { ...payload }, result: null, waitResult: null,
      }),
    };
  }
  if (event.type === "tool_result") {
    for (const field of ["callId", "toolName", "resultState"]) requireString(payload[field], `tool_result ${field}`);
    requireString(payload.callId, "tool_result callId");
    requireString(payload.toolName, "tool_result toolName");
    requireString(payload.resultState, "tool_result resultState");
    if (!Array.isArray(payload.operations)) throw new Error("tool_result operations are invalid");
    const callId = payload.callId;
    const rawResult = {
      ...payload,
      operations: payload.operations.map((operation) => validateOperation(operation, callId)),
    };
    const waitingActivityId = view.agentWaits[callId];
    if (waitingActivityId) {
      const status = ["failed", "denied", "aborted"].includes(payload.resultState) ? "failed" : "completed";
      return {
        ...view,
        activities: view.activities.map((activity) => activity.activityId === waitingActivityId
          ? { ...activity, status, waitResult: rawResult }
          : activity),
      };
    }
    const index = view.activities.findIndex((item) => item.callId === callId);
    if (index < 0 || view.activities[index].toolName !== payload.toolName) throw new Error("tool_result has no matching tool_call");
    const current = view.activities[index];
    let status = ["failed", "denied", "aborted"].includes(payload.resultState) ? "failed" : "completed";
    let childSessionId = current.childSessionId;
    if (current.toolName === "agent") {
      try {
        const result: unknown = JSON.parse(payload.modelContent as string);
        if (isRecord(result) && result.schema === "agent_tool_result_v1" && result.status === "started") {
          status = "running";
          childSessionId = result.childSessionId as string | undefined;
        }
      } catch {
        if (status === "completed") throw new Error("Agent tool result is invalid JSON");
      }
    }
    const activity = {
      ...current,
      status,
      childSessionId,
      result: rawResult,
    };
    return { ...view, activities: view.activities.map((item, itemIndex) => itemIndex === index ? activity : item) };
  }
  if (event.type === "citation_recorded") {
    for (const field of ["citationId", "inputRef", "displayName", "evidenceKind"]) requireString(payload[field], `citation ${field}`);
    requireString(payload.citationId, "citation citationId");
    return {
      ...view,
      // Historical facts are validated; Workspace owns presentation snapshots.
      citations: view.citations,
    };
  }
  if (event.type === "artifact_published") {
    const artifact = artifactLink(payload);
    return { ...view, artifacts: upsertBy(view.artifacts, "artifactRef", artifact) };
  }
  if (event.type === "assistant_message") {
    requireString(payload.messageId, "assistant messageId");
    requireString(payload.modelMarkdown, "assistant modelMarkdown", true);
    if (!Array.isArray(payload.artifactRefs)) throw new Error("assistant artifactRefs are invalid");
    const artifacts = payload.artifactRefs.map((reference) => {
      const artifact = view.artifacts.find((item) => item.artifactRef === reference);
      if (!artifact) throw new Error("assistant references an unknown artifact");
      return artifact;
    });
    const next = commitOverlayBarrier(view, event, sequence);
    return {
      ...next,
      messages: upsertBy(next.messages, "messageId", {
        messageId: payload.messageId, turnId: event.turnId, sequence, role: "assistant", phase: "final",
        status: payload.status, text: payload.modelMarkdown, createdAtMs: event.createdAtMs, attachments: [], artifacts,
      }),
    };
  }
  if (isTerminalSessionEvent(event.type)) {
    const status = event.type === "agent_run_completed" ? "completed" : event.type === "agent_run_failed" ? "failed" : "cancelled";
    return {
      ...view,
      status,
      connection: status,
      finishedAtMs: event.createdAtMs,
      failureReason: event.type === "agent_run_failed" ? payload.reasonType : view.failureReason,
      interruptionReason: event.type === "agent_run_interrupted" ? payload.reasonType : view.interruptionReason,
      live: null,
    };
  }
  return view;
}

function applyLive(
  view: AgentRunViewState,
  live: unknown,
): AgentRunViewState {
  if (live === null || !isAgentRunActive(view)) return view;
  requireObject(live, "live assistant");
  if (!hasExactFields(live, ["afterSequence", "messageId", "revision", "text", "turnId", ...(Object.hasOwn(live, "reasoning") ? ["reasoning"] : [])])) throw new Error("live assistant fields mismatch");
  for (const field of ["messageId", "turnId"]) requireString(live[field], `live ${field}`);
  requireString(live.text, "live text", true);
  if (!isInteger(live.afterSequence) || live.afterSequence < 0 || !isInteger(live.revision) || live.revision <= 0) {
    throw new Error("live assistant sequence is invalid");
  }
  const { messageId, turnId, text, afterSequence, revision } = live;
  requireString(messageId, "live messageId");
  requireString(turnId, "live turnId");
  requireString(text, "live text", true);
  if (!isInteger(afterSequence) || !isInteger(revision)) {
    throw new Error("live assistant sequence is invalid");
  }
  const barrier = Object.hasOwn(view.overlayBarrierByTurnId, turnId)
    ? view.overlayBarrierByTurnId[turnId]
    : 0;
  const reasoning = validateLiveReasoning(live.reasoning);
  const liveAssistant: LiveAssistant = { messageId, turnId, text, afterSequence, revision, ...(reasoning !== undefined ? { reasoning } : {}) };
  if (afterSequence < barrier) return view;
  if (reasoning?.text.trim()) {
    const existing = view.reasoningBlocks.find((block) => block.id === reasoning.blockId);
    if (existing && existing.turnId !== turnId) throw new Error("live reasoning turn identity conflict");
    if (!existing) {
      const anchor = view.reasoningBlocks.reduce((value, block) => block.turnId === turnId ? Math.max(value, block.sequence) : value, afterSequence);
      view = { ...view, reasoningBlocks: [...view.reasoningBlocks, { id: reasoning.blockId, turnId, sequence: anchor + 0.25, text: reasoning.text, status: "streaming" }] };
    }
  }
  if (!text) return { ...view, live: liveAssistant };
  const messages = view.messages.filter((message) => message.messageId !== messageId);
  const answerAnchor = view.reasoningBlocks.reduce((anchor, block) => block.turnId === turnId ? Math.max(anchor, block.sequence) : anchor, afterSequence);
  messages.push({
    messageId, turnId, sequence: answerAnchor + 0.5,
    role: "assistant", phase: "active", status: "streaming", text,
    attachments: [], artifacts: [],
  });
  return { ...view, messages, live: liveAssistant };
}

function projectAgentRun(
  agentRun: RawAgentRun,
  work?: TranscriptProjectionWork,
): ProjectedAgentRun {
  const initialStartedAtMs = Date.parse(agentRun.startedAt || agentRun.createdAt || "");
  let view: AgentRunViewState = {
    ...agentRun,
    reasoningBlocks: [],
    messages: [],
    activities: [],
    citations: agentRun.citations,
    artifacts: [],
    agentWaits: {},
    overlayBarrierByTurnId: {},
    startedAtMs: initialStartedAtMs,
    phaseKey: "thinking",
    phaseStartedAtMs: initialStartedAtMs,
    finishedAtMs: agentRun.completedAt ? Date.parse(agentRun.completedAt) : null,
    connection: ["queued", "running"].includes(agentRun.status) ? (agentRun.streamCursor === "0-0" ? "starting" : "running") : agentRun.status,
    live: null,
    projectionError: null,
  };
  let previousSequence = 0;
  const eventIds = new Set<string>();
  for (const stored of agentRun.events) {
    if (work) work.committedEventVisits += 1;
    requireObject(stored, "stored session event");
    if (!hasExactFields(stored, ["event", "sequence"]) || !Number.isInteger(stored.sequence) || stored.sequence <= previousSequence) {
      throw new Error("stored session event ordering is invalid");
    }
    const event = validateSessionEvent(stored.event, { sessionId: agentRun.sessionId, agentRunId: agentRun.id });
    if (event.sequence !== stored.sequence) throw new Error("stored session event sequence binding mismatch");
    if (eventIds.has(event.eventId)) throw new Error("duplicate session eventId");
    eventIds.add(event.eventId);
    previousSequence = stored.sequence;
    const previousPhaseKey = view.phaseKey;
    view = applySessionEvent(view, event, stored.sequence);
    let runningActivity: ToolActivity | undefined;
    for (let index = view.activities.length - 1; index >= 0; index -= 1) {
      if (view.activities[index].status === "running") {
        runningActivity = view.activities[index];
        break;
      }
    }
    const phaseKey = runningActivity ? `tool:${runningActivity.activityId}` : "thinking";
    view = {
      ...view,
      phaseKey,
      phaseStartedAtMs: event.type === "agent_run_started" || phaseKey !== previousPhaseKey
        ? event.createdAtMs
        : view.phaseStartedAtMs,
    };
  }
  if (work && agentRun.live) work.liveOverlayApplications += 1;
  view = applyLive(view, agentRun.live);
  return { ...view, eventIds: [...eventIds], lastSourceSequence: previousSequence };
}

export function hydrateAgentRun(
  agentRun: unknown,
  identity: AgentRunIdentity = {},
  work?: TranscriptProjectionWork,
): ProjectedAgentRun {
  requireObject(agentRun, "agent run history");
  const fields = ["completedAt", "createdAt", "events", "id", "live", "model", "startedAt", "status", "streamCursor", "citations", "citationSequence"];
  if (!hasExactFields(agentRun, fields) || typeof agentRun.status !== "string" || !AGENT_RUN_STATUSES.has(agentRun.status) || !Array.isArray(agentRun.events)) throw new Error("agent run history fields are invalid");
  requireString(agentRun.id, "agent run id");
  requireString(agentRun.streamCursor, "agent run streamCursor");
  validateCitationSnapshot({ schema: "workspace.citations.v1", sessionId: identity.sessionId || "",
    agentRunId: agentRun.id, throughSequence: agentRun.citationSequence, citations: agentRun.citations }, identity.sessionId || "", agentRun.id);
  const boundAgentRun = {
    ...agentRun,
    ...(identity.sessionId ? { sessionId: identity.sessionId } : {}),
    ...(identity.workspaceId ? { workspaceId: identity.workspaceId } : {}),
  } as RawAgentRun;
  return projectAgentRun(boundAgentRun, work);
}

function quarantineAgentRun(
  agentRun: unknown,
  identity: Required<Pick<AgentRunIdentity, "sessionId" | "workspaceId">>,
): ProjectedAgentRun {
  requireObject(agentRun, "agent run history");
  requireString(agentRun.id, "agent run id");
  if (typeof agentRun.status !== "string" || !AGENT_RUN_STATUSES.has(agentRun.status)) throw new Error("agent run history status is invalid");
  return {
    ...agentRun,
    sessionId: identity.sessionId,
    workspaceId: identity.workspaceId,
    events: [],
    messages: [],
    activities: [],
    reasoningBlocks: [],
    citations: [],
    artifacts: [],
    agentWaits: {},
    overlayBarrierByTurnId: {},
    eventIds: [],
    lastSourceSequence: 0,
    live: null,
    connection: ["queued", "running"].includes(agentRun.status) ? "running" : agentRun.status,
    projectionError: "session_projection_invalid",
  } as unknown as ProjectedAgentRun;
}

export function validateHistoryPage(
  page: unknown,
  expectedIdentity: AgentRunIdentity = {},
  work?: TranscriptProjectionWork,
): HistoryPage {
  const fields = ["agentRuns", "hasMore", "nextCursor", "schema", "session"];
  if (!isRecord(page) || !hasExactFields(page, fields) || page.schema !== HISTORY_PAGE_SCHEMA || !Array.isArray(page.agentRuns)) {
    throw new Error("invalid session history page");
  }
  requireObject(page.session, "history session");
  requireString(page.session.id, "history session id");
  requireString(page.session.workspaceId, "history workspace id");
  if ((expectedIdentity.sessionId && page.session.id !== expectedIdentity.sessionId) || (expectedIdentity.workspaceId && page.session.workspaceId !== expectedIdentity.workspaceId)) {
    throw new Error("session history identity mismatch");
  }
  if (typeof page.hasMore !== "boolean" || (page.nextCursor !== null && typeof page.nextCursor !== "string") || page.hasMore !== Boolean(page.nextCursor)) {
    throw new Error("invalid session history cursor");
  }
  const identity = { sessionId: page.session.id, workspaceId: page.session.workspaceId };
  const agentRuns = page.agentRuns.map((agentRun) => {
    try {
      return hydrateAgentRun(agentRun, identity, work);
    } catch {
      return quarantineAgentRun(agentRun, identity);
    }
  });
  if (new Set(agentRuns.map((agentRun) => agentRun.id)).size !== agentRuns.length) throw new Error("duplicate agentRunId");
  return { ...page, agentRuns } as HistoryPage;
}

export function applyStreamEntry(
  agentRun: ProjectedAgentRun,
  entry: StreamEntry,
  work?: TranscriptProjectionWork,
): ProjectedAgentRun {
  const { cursor, item } = entry;
  if (item.agentRunId !== agentRun.id) throw new Error("Session stream AgentRun binding mismatch");
  const events = agentRun.events || [];
  if (item.kind === "committed") {
    if (agentRun.eventIds?.includes(item.event.eventId)) return { ...agentRun, streamCursor: cursor || agentRun.streamCursor };
    return projectAgentRun({
      ...agentRun,
      events: [...events, { sequence: item.sourceSequence, event: item.event }].sort((left, right) => left.sequence - right.sequence),
      live: isTerminalSessionEvent(item.event.type) ? null : agentRun.live,
      streamCursor: cursor || agentRun.streamCursor,
    }, work);
  }
  if (agentRun.live?.messageId === item.messageId && item.revision <= agentRun.live.revision) {
    return { ...agentRun, streamCursor: cursor || agentRun.streamCursor };
  }
  return projectAgentRun({
    ...agentRun,
    live: {
      messageId: item.messageId,
      turnId: item.turnId,
      afterSequence: item.afterSequence,
      revision: item.revision,
      text: item.text,
      ...(item.reasoning !== undefined ? { reasoning: item.reasoning } : {}),
    },
    streamCursor: cursor || agentRun.streamCursor,
  }, work);
}

export function isAgentRunActive(agentRun: { status: string }) {
  return ["queued", "running"].includes(agentRun.status);
}
