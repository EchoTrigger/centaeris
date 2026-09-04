import type {
  AgentStreamPayload,
  SessionEvent,
} from "../../lib/chatBridge";
import { toPositiveInt } from "./numberUtils";
import {
  compactText,
  formatRuntimeModelError,
  isRecord,
  makeId,
} from "./chatRuntimeCore";
import {
  normalizeToolName,
  parseToolOperations,
} from "./chatToolRuntimeModel";
import type {
  AssistantExecutionTurn,
  EventWaterfall,
  GuidedSupplementChunk,
  NarrativeChunk,
  NarrativeProjectionMeta,
  SubagentChunk,
  SubagentResult,
  SubagentToolGroup,
  TaskChunk,
  TaskResult,
  TaskStatus,
  TranscriptWaterfallSection,
} from "./types";

export const normalizePersistedContent = (raw: unknown): string => {
  if (typeof raw !== "string") {
    throw new Error("session projection message content 必须是 string");
  }
  return raw.trim();
};

export type TerminalSessionEventStatus =
  | "succeeded"
  | "failed"
  | "cancelled"
  | "stopped";

export const getTerminalSessionEventStatus = (
  event: SessionEvent,
): TerminalSessionEventStatus | null => {
  if (event.type === "AgentRunCompleted") {
    return "succeeded";
  }
  if (event.type === "AgentRunFailed") {
    return "failed";
  }
  if (event.type !== "AgentRunInterrupted") {
    return null;
  }
  const payload = getEventPayload(event);
  const reasonType = getEventPayloadString(payload, "reasonType") ?? "";
  if (reasonType === "cancelled") {
    return "cancelled";
  }
  if (["stopped", "shutdown", "provider_interrupted"].includes(reasonType)) {
    return "stopped";
  }
  throw new Error(
    `AgentRunInterrupted 使用了不支持的 reasonType=${reasonType || "<missing>"}。`,
  );
};

export const buildAssistantTurnFromText = (text: string): AssistantExecutionTurn => {
  const normalized = text.trim();
  return {
    id: makeId("assistant-history-turn"),
    chunks: [],
    finalAnswer: normalized,
    isStreaming: false,
  };
};

export const resolvePersistedTaskStatus = (value: unknown): TaskStatus => {
  switch (value) {
    case "queued":
    case "running":
      return "running";
    case "done":
      return "done";
    case "error":
      return "error";
    default:
      throw new Error(`session_event status 不支持: ${String(value ?? "<missing>")}`);
  }
};

export const findPersistedTaskById = (
  turn: AssistantExecutionTurn,
  taskId: string,
): TaskResult | undefined => {
  for (const chunk of turn.chunks) {
    if (chunk.kind === "task" && chunk.task.id === taskId) {
      return chunk.task;
    }
  }
  return undefined;
};

export const findPersistedSubagentById = (
  turn: AssistantExecutionTurn,
  subagentId: string,
): SubagentResult | undefined => {
  for (const chunk of turn.chunks) {
    if (chunk.kind === "subagent" && chunk.subagent.subagentId === subagentId) {
      return chunk.subagent;
    }
  }
  return undefined;
};

export const upsertPersistedTask = (
  turn: AssistantExecutionTurn,
  task: TaskResult,
): AssistantExecutionTurn => {
  let replaced = false;
  const chunks = turn.chunks.map((chunk) => {
    if (chunk.kind !== "task" || chunk.task.id !== task.id) {
      return chunk;
    }
    replaced = true;
    return {
      ...chunk,
      task: {
        ...chunk.task,
        ...task,
      },
    };
  });
  if (!replaced) {
    chunks.push({
      id: makeId("history-task"),
      kind: "task",
      task,
    });
  }
  return {
    ...turn,
    chunks,
  };
};

export const upsertSubagentChunk = (
  turn: AssistantExecutionTurn,
  subagent: SubagentResult,
): AssistantExecutionTurn => {
  let replaced = false;
  const chunks = turn.chunks.map((chunk) => {
    if (
      chunk.kind !== "subagent" ||
      chunk.subagent.subagentId !== subagent.subagentId
    ) {
      return chunk;
    }
    replaced = true;
    return {
      ...chunk,
      subagent: {
        ...chunk.subagent,
        ...subagent,
      },
    };
  });
  if (!replaced) {
    chunks.push({
      id: makeId("subagent"),
      kind: "subagent",
      subagent,
    });
  }
  return {
    ...turn,
    chunks,
  };
};

export const appendGuidedSupplementChunk = (
  turn: AssistantExecutionTurn,
  id: string,
  text: string,
  timestamp: number,
): AssistantExecutionTurn => {
  const normalized = text.trim();
  if (!normalized) {
    return turn;
  }
  if (
    turn.chunks.some(
      (chunk) => chunk.kind === "guidedSupplement" && chunk.id === id,
    )
  ) {
    return turn;
  }
  const chunks = turn.chunks.filter(
    (chunk) =>
      !(
        chunk.kind === "guidedSupplement" &&
        chunk.id.startsWith("optimistic-guided-supplement-") &&
        chunk.text.trim() === normalized
      ),
  );
  return {
    ...turn,
    chunks: [
      ...chunks,
      {
        id,
        kind: "guidedSupplement",
        text: normalized,
        timestamp,
      },
    ],
  };
};

export const upsertPersistedSubagent = (
  turn: AssistantExecutionTurn,
  subagent: SubagentResult,
): AssistantExecutionTurn => {
  let replaced = false;
  const chunks = turn.chunks.map((chunk) => {
    if (
      chunk.kind !== "subagent" ||
      chunk.subagent.subagentId !== subagent.subagentId
    ) {
      return chunk;
    }
    replaced = true;
    return {
      ...chunk,
      subagent: {
        ...chunk.subagent,
        ...subagent,
      },
    };
  });
  if (!replaced) {
    chunks.push({
      id: makeId("history-subagent"),
      kind: "subagent",
      subagent,
    });
  }
  return {
    ...turn,
    chunks,
  };
};

export const hasNarrativeChunk = (
  turn: AssistantExecutionTurn,
  text: string,
  tone: "normal" | "error",
): boolean =>
  turn.chunks.some(
    (chunk) =>
      chunk.kind === "narrative" && chunk.text === text && chunk.tone === tone,
  );

export const appendPersistedNarrative = (
  turn: AssistantExecutionTurn,
  text: string,
  tone: "normal" | "error" = "normal",
  turnId?: string,
  waterfall?: EventWaterfall,
  projectionMeta?: NarrativeProjectionMeta,
): AssistantExecutionTurn => {
  const normalized = text.trim();
  if (!normalized) {
    return turn;
  }
  if (hasNarrativeChunk(turn, normalized, tone)) {
    return turn;
  }
  const previous =
    turn.chunks.length > 0 ? turn.chunks[turn.chunks.length - 1] : null;
  if (
    previous &&
    previous.kind === "narrative" &&
    previous.text === normalized &&
    previous.tone === tone &&
    previous.turnId === turnId
  ) {
    return turn;
  }
  return {
    ...turn,
    chunks: [
      ...turn.chunks,
      {
        id: makeId("history-narrative"),
        kind: "narrative",
        turnId,
        text: normalized,
        phase: projectionMeta?.phase,
        ephemeral: projectionMeta?.ephemeral,
        scope: projectionMeta?.scope,
        streamKey: projectionMeta?.streamKey,
        sourceItemId: projectionMeta?.sourceItemId,
        tone,
        waterfall,
      },
    ],
  };
};

export const flushPersistedDraftAnswerToNarrative = (
  turn: AssistantExecutionTurn,
  turnId?: string,
): AssistantExecutionTurn => {
  const draft = turn.finalAnswer.trim();
  if (!draft) {
    return turn;
  }
  return appendPersistedNarrative(
    {
      ...turn,
      finalAnswer: "",
    },
    draft,
    "normal",
    turnId,
  );
};

export const normalizePersistedStreamPayloads = (
  rawItems: unknown,
): AgentStreamPayload[] => {
  if (!Array.isArray(rawItems)) {
    return [];
  }
  return rawItems.filter((item): item is AgentStreamPayload => isRecord(item));
};

const formatStreamPayloadType = (value: unknown): string =>
  typeof value === "string" && value.trim() ? value.trim() : "<missing>";

const isSupportedRestoreStreamPayloadType = (value: unknown): boolean => {
  const itemType = formatStreamPayloadType(value);
  return itemType === "session_event" || itemType === "error";
};

export const assertProjectionStreamPayloads = (
  taskId: string,
  rawItems: unknown[],
): AgentStreamPayload[] =>
  rawItems.map((item, index) => {
    if (!isRecord(item)) {
      throw new Error(
        `历史恢复失败：session projection task ${taskId} item ${index} 非法`,
      );
    }
    const itemType = typeof item.type === "string" ? item.type : "";
    if (!isSupportedRestoreStreamPayloadType(itemType)) {
      throw new Error(
        `历史恢复失败：session projection task ${taskId} item ${index} 不支持 stream payload type=${formatStreamPayloadType(itemType)}`,
      );
    }
    return item as AgentStreamPayload;
  });

export const getSessionEventId = (event: SessionEvent): string => {
  if (typeof event.id === "string" && event.id.trim()) {
    return event.id.trim();
  }
  const payload = isRecord(event.payload) ? event.payload : {};
  const stableParts = [
    typeof event.type === "string" ? event.type.trim() : "",
    typeof event.sessionId === "string" ? event.sessionId.trim() : "",
    typeof event.turnId === "string" ? event.turnId.trim() : "",
    typeof event.taskId === "string" ? event.taskId.trim() : "",
    typeof payload.callId === "string" ? payload.callId.trim() : "",
    typeof payload.stage === "string" ? payload.stage.trim() : "",
    typeof event.status === "string" ? event.status.trim() : "",
  ].filter(Boolean);
  return stableParts.length > 0 ? stableParts.join(":") : "";
};

export const getEventTurnId = (event: SessionEvent): string | undefined => {
  const turnId = typeof event.turnId === "string" ? event.turnId.trim() : "";
  return turnId || undefined;
};

const getEventPayload = (event: SessionEvent): Record<string, unknown> =>
  isRecord(event.payload) ? event.payload : {};

const getEventPayloadString = (
  payload: Record<string, unknown>,
  field: string,
): string | undefined => {
  const value = payload[field];
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
};

const getEventPayloadRawString = (
  payload: Record<string, unknown>,
  field: string,
): string => {
  const value = payload[field];
  return typeof value === "string" ? value : "";
};

const getEventAtMs = (event: SessionEvent): number =>
  typeof event.at === "number" && Number.isFinite(event.at)
    ? event.at
    : Date.now();

const resolveEventTaskId = (event: SessionEvent): string => {
  const payload = getEventPayload(event);
  const callId = getEventPayloadString(payload, "callId");
  if (callId) {
    return callId;
  }
  throw new Error(`session_event ${event.type || "<missing>"} 缺少 callId`);
};

const resolveEventStatus = (event: SessionEvent): TaskStatus =>
  resolvePersistedTaskStatus(event.status);

const buildTaskFromSessionEvent = (
  event: SessionEvent,
  existingTask: TaskResult | undefined,
): TaskResult => {
  const payload = getEventPayload(event);
  const taskId = resolveEventTaskId(event);
  const toolName = normalizeToolName(event.toolName);
  if (existingTask && existingTask.title !== toolName) {
    throw new Error(`session_event ${event.type} toolName identity 不匹配`);
  }
  if (event.type === "ToolResult" && !existingTask) {
    throw new Error("ToolResult 没有匹配的 ToolCall");
  }
  const summary =
    getEventPayloadString(payload, "summary") ||
    existingTask?.summary ||
    toolName;
  const eventStatus = resolveEventStatus(event);
  const liveCommand = toolName === "bash"
    ? getEventPayloadString(payload, "command")
    : "";
  const liveDescription = toolName === "bash"
    ? getEventPayloadString(payload, "description")
    : "";
  const normalizedInput = isRecord(payload.normalizedInput)
    ? { ...payload.normalizedInput }
    : liveCommand || liveDescription
      ? {
          ...existingTask?.normalizedInput,
          ...(liveCommand ? { command: liveCommand } : {}),
          ...(liveDescription ? { description: liveDescription } : {}),
        }
      : existingTask?.normalizedInput;
  const displayTarget =
    getEventPayloadString(payload, "displayTarget") || existingTask?.displayTarget;
  let operations = existingTask?.operations;
  if (event.type === "ToolResult") {
    const parsed = parseToolOperations(payload.operations, {
      callId: taskId,
      toolName,
    });
    operations = parsed;
  } else if (!operations) {
    operations = [{ callId: taskId, toolName, status: eventStatus }];
  }
  return {
    id: taskId,
    turnId: getEventTurnId(event) || existingTask?.turnId,
    title: toolName,
    summary,
    status: eventStatus,
    provider: "tool",
    durationMs:
      typeof payload.latencyMs === "number"
        ? Math.max(0, payload.latencyMs)
        : existingTask?.durationMs,
    operations,
    normalizedInput,
    displayTarget,
    modelContent:
      event.type === "ToolResult"
        ? getEventPayloadRawString(payload, "modelContent")
        : existingTask?.modelContent,
    fullOutputPath:
      event.type === "ToolResult"
        ? getEventPayloadString(payload, "fullOutputPath")
        : existingTask?.fullOutputPath,
    outputStartByte:
      event.type === "ToolResult" && typeof payload.outputStartByte === "number"
        ? payload.outputStartByte
        : existingTask?.outputStartByte,
    outputByteLength:
      event.type === "ToolResult" && typeof payload.outputByteLength === "number"
        ? payload.outputByteLength
        : existingTask?.outputByteLength,
    waterfall: existingTask?.waterfall,
  };
};

const normalizeEventStringArray = (value: unknown): string[] | undefined =>
  normalizeStringArray(value);

const buildSubagentToolGroupFromEvent = (
  event: SessionEvent,
  payload: Record<string, unknown>,
): SubagentToolGroup | undefined => {
  const toolGroupId = getEventPayloadString(payload, "toolGroupId");
  if (!toolGroupId) {
    return undefined;
  }
  const title =
    getEventPayloadString(payload, "title") ||
    "子任务工具执行";
  const summary =
    getEventPayloadString(payload, "summary") ||
    getEventPayloadString(payload, "message") ||
    title;
  return {
    id: toolGroupId,
    title,
    summary,
    status: resolveEventStatus(event),
    stats: isRecord(payload.stats) ? payload.stats : undefined,
    details: payload.details,
    sourceEventIds:
      normalizeEventStringArray(payload.sourceEventIds) ||
      normalizeEventStringArray(event.id ? [event.id] : undefined),
  };
};

const resolveEventSubagentId = (event: SessionEvent): string => {
  const payload = getEventPayload(event);
  return (
    getEventPayloadString(payload, "subagentId") ||
    (typeof event.taskId === "string" && event.taskId.trim()) ||
    getSessionEventId(event) ||
    makeId("subagent")
  );
};

const buildSubagentFromSessionEvent = (
  event: SessionEvent,
  existingSubagent: SubagentResult | undefined,
): SubagentResult => {
  const payload = getEventPayload(event);
  const workPacketSummary = isRecord(payload.workPacketSummary)
    ? payload.workPacketSummary
    : undefined;
  const resultEnvelope = isRecord(payload.resultEnvelope)
    ? payload.resultEnvelope
    : undefined;
  const toolGroup = buildSubagentToolGroupFromEvent(event, payload);
  const isToolGroupUpdate = Boolean(toolGroup);
  const incomingStatus = resolveEventStatus(event);
  const status = isToolGroupUpdate
    ? existingSubagent?.status ||
    (incomingStatus === "error" ? "error" : "running")
    : incomingStatus;
  const subagentId = resolveEventSubagentId(event);
  const title =
    (!isToolGroupUpdate && getEventPayloadString(payload, "title")) ||
    existingSubagent?.title ||
    "协作代理";
  const summary =
    (!isToolGroupUpdate && getEventPayloadString(payload, "summary")) ||
    previewRecord(resultEnvelope, 180) ||
    existingSubagent?.summary ||
    (status === "running" ? "正在处理子任务" : "子任务已更新");
  const incomingDescription =
    !isToolGroupUpdate ? getEventPayloadString(payload, "description") || "" : "";
  const incomingDescriptionIsLifecycleSummary =
    Boolean(incomingDescription) &&
    (incomingDescription === summary ||
      incomingDescription === getEventPayloadString(payload, "summary"));
  const taskDescription =
    previewRecord(workPacketSummary, 260) ||
    (!incomingDescriptionIsLifecycleSummary ? incomingDescription : "");
  const description =
    taskDescription || existingSubagent?.description || incomingDescription || summary;
  const resultPreview =
    previewRecord(resultEnvelope) ||
    getEventPayloadString(payload, "message") ||
    existingSubagent?.resultPreview;
  const incomingStartedAtMs = toPositiveInt(payload.startedAtMs);
  const incomingCompletedAtMs =
    toPositiveInt(payload.completedAtMs) ??
    (status === "running" ? undefined : toPositiveInt(event.at));
  const childSessionId =
    getEventPayloadString(payload, "childSessionId") ||
    (isRecord(payload.taskNotification)
      ? getEventPayloadString(payload.taskNotification, "childSessionId")
      : "") ||
    existingSubagent?.childSessionId;
  return {
    id: getSessionEventId(event) || existingSubagent?.id || makeId("subagent"),
    subagentId,
    turnId: getEventTurnId(event) || existingSubagent?.turnId,
    taskId: typeof event.taskId === "string" ? event.taskId : existingSubagent?.taskId,
    parentTaskId: event.parentTaskId ?? existingSubagent?.parentTaskId,
    childSessionId,
    displayName: title,
    avatarSeed: childSessionId || existingSubagent?.avatarSeed,
    role:
      getEventPayloadString(payload, "role") ||
      existingSubagent?.role,
    title,
    summary,
    description,
    status,
    resultPreview,
    startedAtMs: existingSubagent?.startedAtMs ?? incomingStartedAtMs,
    completedAtMs:
      status === "running"
        ? undefined
        : incomingCompletedAtMs ?? existingSubagent?.completedAtMs,
    workPacketSummary: workPacketSummary || existingSubagent?.workPacketSummary,
    resultEnvelope: resultEnvelope || existingSubagent?.resultEnvelope,
    payload,
    toolGroups: mergeSubagentToolGroups(
      existingSubagent?.toolGroups,
      toolGroup,
    ),
    waterfall: existingSubagent?.waterfall,
  };
};

export const applySessionEventToAssistantTurn = (
  turn: AssistantExecutionTurn,
  event: SessionEvent,
): AssistantExecutionTurn => {
  if (getTerminalSessionEventStatus(event)) {
    return {
      ...turn,
      isStreaming: false,
      activity: undefined,
      completedAtMs:
        typeof event.at === "number" && Number.isFinite(event.at)
          ? event.at
          : turn.completedAtMs,
    };
  }
  if (
    typeof event.visibility === "string" &&
    event.visibility.toLowerCase() !== "user"
  ) {
    return turn;
  }
  const payload = getEventPayload(event);
  const eventTurnId = getEventTurnId(event);
  switch (event.type) {
    case "ModelTextDelta": {
      const delta = getEventPayloadRawString(payload, "delta");
      return delta ? { ...turn, finalAnswer: `${turn.finalAnswer}${delta}` } : turn;
    }
    case "ModelTextReplace": {
      const content = getEventPayloadRawString(payload, "content");
      return { ...turn, finalAnswer: content };
    }
    case "Final": {
      const content = getEventPayloadRawString(payload, "content");
      return content ? { ...turn, finalAnswer: content } : turn;
    }
    case "Status": {
      if (getEventPayloadString(payload, "stage") !== "model_process_summary") {
        return turn;
      }
      const text = getEventPayloadString(payload, "message");
      if (!text) {
        return turn;
      }
      const nextTurn = turn.finalAnswer.trim()
        ? flushPersistedDraftAnswerToNarrative(turn, eventTurnId)
        : turn;
      return appendPersistedNarrative(nextTurn, text, "normal", eventTurnId);
    }
    case "PromptCompaction": {
      if (event.status !== "done") {
        throw new Error("用户时间线只接受已提交的 PromptCompaction");
      }
      const sourceItemId = getSessionEventId(event);
      if (
        turn.chunks.some(
          (chunk) =>
            chunk.kind === "narrative" &&
            chunk.phase === "compaction" &&
            chunk.sourceItemId === sourceItemId,
        )
      ) {
        return turn;
      }
      const nextTurn = flushPersistedDraftAnswerToNarrative(turn, eventTurnId);
      return {
        ...nextTurn,
        activity: null,
        chunks: [
          ...nextTurn.chunks,
          {
            id: `compaction-${sourceItemId}`,
            kind: "narrative",
            turnId: eventTurnId,
            text: "Compacted conversation",
            phase: "compaction",
            sourceItemId,
            tone: "normal",
          },
        ],
      };
    }
    case "ModelRequestStart":
    case "ModelStatus":
    case "ToolCallPreparing":
    case "ToolCallReady":
    case "ToolProgress":
    case "PermissionRequired":
    case "QuestionRequired":
    case "AgentRunInterventionChanged":
    case "RuntimeWaitChanged":
    case "Citation":
    case "Artifact":
      return turn;
    case "TurnSupplement": {
      const text = getEventPayloadString(payload, "message") || "";
      return appendGuidedSupplementChunk(
        turn,
        getSessionEventId(event) || makeId("guided-supplement"),
        text,
        getEventAtMs(event),
      );
    }
    case "ToolCall":
    case "ToolResult": {
      if (!event.toolName) {
        throw new Error(`session_event ${event.type} 缺少 toolName`);
      }
      const nextTurn = flushPersistedDraftAnswerToNarrative(turn, eventTurnId);
      const taskId = resolveEventTaskId(event);
      const existingTask = findPersistedTaskById(nextTurn, taskId);
      return upsertPersistedTask(
        nextTurn,
        buildTaskFromSessionEvent(event, existingTask),
      );
    }
    case "SubagentSpawned":
    case "SubagentProgress":
    case "SubagentToolGroup":
    case "SubagentResult":
    case "SubagentFailed":
    case "SubagentCancelled": {
      const nextTurn = flushPersistedDraftAnswerToNarrative(turn, eventTurnId);
      const subagentId = resolveEventSubagentId(event);
      const existingSubagent = findPersistedSubagentById(nextTurn, subagentId);
      return upsertPersistedSubagent(
        nextTurn,
        buildSubagentFromSessionEvent(event, existingSubagent),
      );
    }
    case "RuntimeError": {
      return appendPersistedNarrative(
        turn,
        formatRuntimeModelError(payload),
        "error",
        eventTurnId,
      );
    }
    default:
      throw new Error(
        `session/runtime event type is unsupported: ${event.type || "<missing>"}`,
      );
  }
};

export const getChunkWaterfallSection = (
  chunk: NarrativeChunk | GuidedSupplementChunk | TaskChunk | SubagentChunk,
): TranscriptWaterfallSection => {
  const waterfall =
    chunk.kind === "task"
      ? chunk.task.waterfall
      : chunk.kind === "subagent"
        ? chunk.subagent.waterfall
        : chunk.waterfall;
  const section =
    typeof waterfall?.section === "string" ? waterfall.section.trim() : "";
  if (
    section === "process" ||
    section === "tool" ||
    section === "subagent" ||
    section === "final"
  ) {
    return section;
  }
  if (chunk.kind === "task") {
    return "tool";
  }
  if (chunk.kind === "subagent") {
    return "subagent";
  }
  return "process";
};

export const getChunkWaterfallOrder = (
  chunk: NarrativeChunk | GuidedSupplementChunk | TaskChunk | SubagentChunk,
): number => {
  const waterfall =
    chunk.kind === "task"
      ? chunk.task.waterfall
      : chunk.kind === "subagent"
        ? chunk.subagent.waterfall
        : chunk.waterfall;
  const order = waterfall?.order;
  if (typeof order === "number" && Number.isFinite(order)) {
    return order;
  }
  const section = getChunkWaterfallSection(chunk);
  if (section === "tool") {
    return 20;
  }
  if (section === "subagent") {
    return 25;
  }
  return 10;
};

export const previewRecord = (value: unknown, maxLength: number = 220): string => {
  if (!isRecord(value)) {
    return "";
  }
  const summary = typeof value.summary === "string" ? value.summary.trim() : "";
  if (summary) {
    return compactText(summary, maxLength);
  }
  const findings = Array.isArray(value.findings)
    ? value.findings.filter(
      (item): item is string =>
        typeof item === "string" && item.trim().length > 0,
    )
    : [];
  if (findings.length > 0) {
    return compactText(findings.slice(0, 3).join("；"), maxLength);
  }
  try {
    return compactText(JSON.stringify(value), maxLength);
  } catch {
    return "";
  }
};

export const normalizeStringArray = (value: unknown): string[] | undefined => {
  if (!Array.isArray(value)) {
    return undefined;
  }
  const items = value
    .filter(
      (item): item is string =>
        typeof item === "string" && item.trim().length > 0,
    )
    .map((item) => item.trim());
  return items.length > 0 ? items : undefined;
};

export const mergeSubagentToolGroups = (
  existing: SubagentToolGroup[] | undefined,
  next: SubagentToolGroup | undefined,
): SubagentToolGroup[] | undefined => {
  if (!next) {
    return existing;
  }
  const groups = existing ? [...existing] : [];
  const index = groups.findIndex((group) => group.id === next.id);
  if (index >= 0) {
    groups[index] = {
      ...groups[index],
      ...next,
      stats: next.stats || groups[index].stats,
      details: next.details ?? groups[index].details,
      sourceEventIds: next.sourceEventIds || groups[index].sourceEventIds,
    };
    return groups;
  }
  groups.push(next);
  return groups;
};

export const buildAssistantTurnFromStreamItems = (
  rawItems: unknown,
  fallbackText: string,
  fallbackTimestamp?: number,
): AssistantExecutionTurn => {
  const streamItems = normalizePersistedStreamPayloads(rawItems);
  if (streamItems.length === 0) {
    return buildAssistantTurnFromText(fallbackText);
  }

  let turn: AssistantExecutionTurn = {
    id: makeId("assistant-history-turn"),
    chunks: [],
    finalAnswer: "",
    isStreaming: false,
  };
  const seenSessionEventIds = new Set<string>();
  const observedAtMs: number[] = [];

  for (const item of streamItems) {
    if (!isSupportedRestoreStreamPayloadType(item.type)) {
      throw new Error(
        `历史恢复失败：不支持 stream payload type=${formatStreamPayloadType(item.type)}`,
      );
    }

    if (item.type === "session_event" && isRecord(item.event)) {
      const event = item.event as SessionEvent;
      const eventId = getSessionEventId(event);
      if (eventId) {
        if (seenSessionEventIds.has(eventId)) {
          continue;
        }
        seenSessionEventIds.add(eventId);
      }
      if (typeof event.at === "number" && Number.isFinite(event.at)) {
        observedAtMs.push(event.at);
      }
      turn = applySessionEventToAssistantTurn(turn, event);
      continue;
    }

    if (item.type === "error") {
      const text =
        (typeof item.message === "string" && item.message.trim()) ||
        (typeof item.content === "string" && item.content.trim()) ||
        (typeof item.text === "string" && item.text.trim()) ||
        "处理异常";
      turn = appendPersistedNarrative(turn, text, "error");
      continue;
    }
  }

  const fallback = fallbackText.trim();
  if (!turn.finalAnswer.trim() && fallback) {
    turn = {
      ...turn,
      finalAnswer: fallback,
    };
  }
  if (turn.chunks.length === 0 && !turn.finalAnswer.trim()) {
    return buildAssistantTurnFromText(fallbackText);
  }
  const normalizedObservedAt = observedAtMs.filter((value) => value > 0);
  const startedAtMs =
    normalizedObservedAt.length > 0
      ? Math.min(...normalizedObservedAt)
      : undefined;
  const completedAtMs =
    normalizedObservedAt.length > 0
      ? Math.max(...normalizedObservedAt)
      : typeof fallbackTimestamp === "number"
        ? fallbackTimestamp
        : undefined;
  return {
    ...turn,
    startedAtMs,
    completedAtMs,
  };
};

export const isPersistedAssistantErrorStatus = (status: string): boolean =>
  status === "error";

export const applyPersistedAssistantStatusToTurn = (
  turn: AssistantExecutionTurn,
  status: string,
  fallbackText: string,
  preserveAnswerOnError: boolean,
  turnId?: string,
): AssistantExecutionTurn => {
  if (!isPersistedAssistantErrorStatus(status)) {
    return turn;
  }
  const shouldPreserveFinalAnswer =
    preserveAnswerOnError && Boolean(turn.finalAnswer.trim());
  const completedTurn = {
    ...turn,
    isStreaming: false,
    activity: undefined,
  };
  const fallback = fallbackText.trim();
  if (fallback && turn.finalAnswer.trim() === fallback) {
    return appendPersistedNarrative(
      { ...completedTurn, finalAnswer: "" },
      fallback,
      "error",
      turnId,
    );
  }
  if (
    turn.chunks.some(
      (chunk) => chunk.kind === "narrative" && chunk.tone === "error",
    )
  ) {
    return shouldPreserveFinalAnswer
      ? completedTurn
      : {
        ...completedTurn,
        finalAnswer: "",
      };
  }
  const errorText =
    fallback ||
    (shouldPreserveFinalAnswer ? "处理异常" : turn.finalAnswer.trim()) ||
    "处理异常";
  const baseTurn = shouldPreserveFinalAnswer
    ? completedTurn
    : {
      ...completedTurn,
      finalAnswer: "",
    };
  return appendPersistedNarrative(
    baseTurn,
    errorText,
    "error",
    turnId,
  );
};
