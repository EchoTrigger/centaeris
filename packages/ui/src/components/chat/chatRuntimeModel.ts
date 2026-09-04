import {
  getAgentContextUsage,
  getAgentRuntimeConfig,
  getAgentState,
  getSessionProjection,
  replayAgentRunStream,
  type AgentStreamPayload,
  type AgentRunSummary,
  type SessionData,
  type SessionProjectionData,
  type SessionEvent,
  type PendingQuestionSummary,
  type PersistedChatMessage,
} from "../../lib/chatBridge";
import {
  deriveReplayCursorPatch,
  type SessionReplayCursors,
} from "../../lib/sessionViewCache";
import type {
  AssistantExecutionTurn,
  ChatMessage,
  NarrativeChunk,
  PendingQuestionRequest,
  SessionHydrationSnapshot,
  StreamSeenSets,
  SubagentChunk,
  TaskChunk,
  AgentRunReplaySnapshot,
} from "./types";
import {
  DEFAULT_RUNTIME_ACTIVITY,
  buildPendingTurn,
  formatExecutionError,
  isRecord,
  makeId,
  readAutoContinueAfterResumeWaitPreference,
} from "./chatRuntimeCore";
import {
  applyPersistedAssistantStatusToTurn,
  buildAssistantTurnFromStreamItems,
  assertProjectionStreamPayloads,
  getSessionEventId,
  normalizePersistedContent,
  normalizePersistedStreamPayloads,
} from "./chatTranscriptRestore";
export {
  appendGuidedSupplementChunk,
  appendPersistedNarrative,
  applySessionEventToAssistantTurn,
  applyPersistedAssistantStatusToTurn,
  assertProjectionStreamPayloads,
  buildAssistantTurnFromStreamItems,
  buildAssistantTurnFromText,
  findPersistedSubagentById,
  findPersistedTaskById,
  flushPersistedDraftAnswerToNarrative,
  getChunkWaterfallOrder,
  getChunkWaterfallSection,
  getEventTurnId,
  getSessionEventId,
  getTerminalSessionEventStatus,
  hasNarrativeChunk,
  isPersistedAssistantErrorStatus,
  mergeSubagentToolGroups,
  normalizePersistedContent,
  normalizePersistedStreamPayloads,
  previewRecord,
  resolvePersistedTaskStatus,
  upsertPersistedSubagent,
  upsertPersistedTask,
  upsertSubagentChunk,
} from "./chatTranscriptRestore";

export {
  normalizeToolName,
  normalizeToolOperation,
  parseToolOperations,
  toDisplayPath,
  toLineRange,
} from "./chatToolRuntimeModel";

export {
  AUTO_CONTINUE_AFTER_RESUME_WAIT_KEY,
  DEFAULT_RUNTIME_ACTIVITY,
  EMPTY_MODEL_RUNTIME_DRAFT,
  RUNTIME_ACTIVITY_BY_PROCESS_STATE,
  buildContextUsageTooltip,
  buildModelRuntimeDraft,
  buildPendingTurn,
  compactText,
  formatExecutionError,
  formatRuntimeModelError,
  formatUserMessageTimestamp,
  isRecord,
  makeId,
  mapPreparingToolNameToActivity,
  mapProcessStateToActivity,
  mapRuntimePayloadToActivity,
  normalizeRuntimeActivity,
  normalizeRuntimeProcessState,
  parseJsonLike,
  readAutoContinueAfterResumeWaitPreference,
  sanitizeRuntimeActivityLabel,
  sessionViewCacheStore,
  taskStatusLabel,
  toRuntimeDraftNumber,
} from "./chatRuntimeCore";


export const parsePendingQuestionRequest = (
  raw: unknown,
): PendingQuestionRequest | null => {
  if (!isRecord(raw)) {
    return null;
  }
  const id = typeof raw.id === "string" ? raw.id.trim() : "";
  const question = typeof raw.question === "string" ? raw.question.trim() : "";
  if (!id || !question) {
    return null;
  }
  const options = Array.isArray(raw.options)
    ? raw.options.filter(
      (item): item is string =>
        typeof item === "string" && item.trim().length > 0,
    )
    : [];
  return {
    id,
    question,
    options,
    multiSelect: Boolean(raw.multiSelect),
    required: raw.required !== false,
  };
};

export const SESSION_PROJECTION_SCHEMA_VERSION = "session_projection.v1";
export const AGENT_RUN_STREAM_REPLAY_PAGE_SIZE = 2_000;
export const AGENT_RUN_STREAM_REPLAY_MAX_PAGES = 128;


export const normalizeAgentRunId = (value: unknown): string =>
  typeof value === "string" ? value.trim() : "";

export const resolveHistoryTimestamp = (rawMessage: PersistedChatMessage): number =>
  typeof rawMessage.createdAtMs === "number"
    ? rawMessage.createdAtMs
    : typeof rawMessage.updatedAtMs === "number"
      ? rawMessage.updatedAtMs
      : 0;

export const resolveHistoryMessageId = (
  rawMessage: PersistedChatMessage,
  role: string,
  timestamp: number,
  index: number,
): string =>
  typeof rawMessage.id === "string" && rawMessage.id.trim()
    ? rawMessage.id.trim()
    : `history-${role}-${timestamp}-${index}`;

export const collectSessionVisibleMessageIds = (
  sessionData: SessionData | null,
): string[] => {
  const messageIds: string[] = [];
  const rawMessages = Array.isArray(sessionData?.messages)
    ? sessionData.messages
    : [];
  rawMessages.forEach((rawMessage: PersistedChatMessage, index: number) => {
    const role =
      typeof rawMessage.role === "string" ? rawMessage.role.trim() : "";
    if (role !== "user" && role !== "assistant") {
      return;
    }
    const timestamp = resolveHistoryTimestamp(rawMessage);
    const content = normalizePersistedContent(rawMessage.content);
    if (role === "user") {
      const hasImage = Array.isArray(rawMessage.imageData)
        ? rawMessage.imageData.length > 0
        : Boolean(rawMessage.imageData);
      if (!content && !hasImage) {
        return;
      }
      messageIds.push(resolveHistoryMessageId(rawMessage, role, timestamp, index));
      return;
    }

    const status =
      typeof rawMessage.status === "string"
        ? rawMessage.status.trim().toLowerCase()
        : "";
    const isRunningAssistant =
      status === "running" ||
      status === "queued" ||
      status === "waiting_user";
    const hasTask = Boolean(normalizeAgentRunId(rawMessage.agentRunId));
    if (!content && !hasTask && !isRunningAssistant && status !== "error") {
      return;
    }
    messageIds.push(resolveHistoryMessageId(rawMessage, role, timestamp, index));
  });
  return messageIds;
};

export const findAssistantHistoryMessageIdForAgentRun = (
  sessionData: SessionData | null,
  agentRunId: string,
): string | null => {
  const normalizedAgentRunId = normalizeAgentRunId(agentRunId);
  if (!normalizedAgentRunId) {
    return null;
  }
  const rawMessages = Array.isArray(sessionData?.messages)
    ? sessionData.messages
    : [];
  for (let index = 0; index < rawMessages.length; index += 1) {
    const rawMessage = rawMessages[index];
    const role =
      typeof rawMessage.role === "string" ? rawMessage.role.trim() : "";
    if (role !== "assistant") {
      continue;
    }
    if (normalizeAgentRunId(rawMessage.agentRunId) !== normalizedAgentRunId) {
      continue;
    }
    const timestamp = resolveHistoryTimestamp(rawMessage);
    return resolveHistoryMessageId(rawMessage, role, timestamp, index);
  }
  return null;
};

export const buildHistoryMessages = (
  _sessionTitle: string,
  sessionData: SessionData | null,
  streamItemsByAgentRunId: ReadonlyMap<string, AgentStreamPayload[]> = new Map(),
  agentRunsById: ReadonlyMap<string, AgentRunSummary> = new Map(),
): ChatMessage[] => {
  const rawMessages = Array.isArray(sessionData?.messages)
    ? sessionData.messages
    : [];
  const restored: ChatMessage[] = [];
  rawMessages.forEach((rawMessage: PersistedChatMessage, index: number) => {
    appendHistoryMessage(
      restored,
      rawMessage,
      index,
      streamItemsByAgentRunId,
      agentRunsById,
    );
  });

  if (restored.length > 0) {
    return restored;
  }

  return [];
};

const appendHistoryMessage = (
  restored: ChatMessage[],
  rawMessage: PersistedChatMessage,
  index: number,
  streamItemsByAgentRunId: ReadonlyMap<string, AgentStreamPayload[]>,
  agentRunsById: ReadonlyMap<string, AgentRunSummary>,
): void => {
  const role =
    typeof rawMessage.role === "string" ? rawMessage.role.trim() : "";
  const timestamp = resolveHistoryTimestamp(rawMessage);
  const content = normalizePersistedContent(rawMessage.content);
  const messageId =
    typeof rawMessage.id === "string" && rawMessage.id.trim()
      ? rawMessage.id.trim()
      : `history-${role}-${timestamp}-${index}`;
  if (role === "user") {
    const hasImage = Array.isArray(rawMessage.imageData)
      ? rawMessage.imageData.length > 0
      : Boolean(rawMessage.imageData);
    const text = content || (hasImage ? "[图片消息]" : "");
    if (!text) {
      return;
    }
    restored.push({
      id: messageId,
      role: "user",
      text,
      timestamp,
    });
    return;
  }
  if (role !== "assistant") {
    return;
  }
  const status =
    typeof rawMessage.status === "string"
      ? rawMessage.status.trim().toLowerCase()
      : "";
  const agentRunId = normalizeAgentRunId(rawMessage.agentRunId);
  if (!agentRunId) {
    throw new Error(`历史消息 ${messageId} 缺少 agentRunId`);
  }
  const taskSummary = agentRunsById.get(agentRunId);
  const taskStatus = normalizeAgentRunStatus(taskSummary?.status);
  const taskIsTerminal = taskStatus
    ? isTerminalAgentRunStatus(taskStatus)
    : false;
  const isRunningAssistant =
    !taskIsTerminal &&
    (status === "running" ||
      status === "queued" ||
      status === "waiting_user" ||
      (taskStatus ? isActiveAgentRunStatus(taskStatus) : false));
  const streamItems = streamItemsByAgentRunId.get(agentRunId);
  const hasStreamReplay = Boolean(streamItems && streamItems.length > 0);
  if (!content && !hasStreamReplay && !isRunningAssistant && status !== "error") {
    return;
  }
  const baseTurn = hasStreamReplay
    ? buildAssistantTurnFromStreamItems(streamItems, content, timestamp)
    : isRunningAssistant
      ? buildPendingTurn()
      : buildAssistantTurnFromStreamItems(undefined, content, timestamp);
  const restoredTurn = {
    ...baseTurn,
    agentRunId,
    id: rawMessage.turnId || baseTurn.id || makeId("assistant-history-turn"),
    isStreaming: isRunningAssistant ? true : baseTurn.isStreaming,
    startedAtMs: taskSummary?.startedAtMs ?? baseTurn.startedAtMs ?? timestamp,
    completedAtMs: isRunningAssistant
      ? baseTurn.completedAtMs
      : (taskSummary?.completedAtMs ??
        taskSummary?.updatedAtMs ??
        baseTurn.completedAtMs),
    activity: isRunningAssistant
      ? (baseTurn.activity ?? DEFAULT_RUNTIME_ACTIVITY)
      : baseTurn.activity,
  };
  const persistedStatus =
    taskStatus === "failed"
      ? "error"
      : taskIsTerminal && status === "running"
        ? taskStatus
        : status;
  const turn = applyPersistedAssistantStatusToTurn(
    restoredTurn,
    persistedStatus,
    content,
    hasStreamReplay,
    rawMessage.turnId,
  );
  restored.push({
    id: messageId,
    role: "assistant",
    status: persistedStatus,
    turn,
  });
};

export const buildHistoryMessagesChunked = async (
  _sessionTitle: string,
  sessionData: SessionData | null,
  streamItemsByAgentRunId: ReadonlyMap<string, AgentStreamPayload[]> = new Map(),
  agentRunsById: ReadonlyMap<string, AgentRunSummary> = new Map(),
  control?: HydrationControl,
): Promise<ChatMessage[]> => {
  const rawMessages = Array.isArray(sessionData?.messages)
    ? sessionData.messages
    : [];
  const restored: ChatMessage[] = [];
  for (let index = 0; index < rawMessages.length; index += 1) {
    assertHydrationNotCancelled(control);
    appendHistoryMessage(
      restored,
      rawMessages[index],
      index,
      streamItemsByAgentRunId,
      agentRunsById,
    );
    if ((index + 1) % HYDRATION_MESSAGE_BATCH_SIZE === 0) {
      await yieldHydration(control);
    }
  }
  return restored;
};

export const normalizeAgentRunStatus = (status: unknown): string =>
  typeof status === "string" ? status.trim().toLowerCase() : "";

export const ACTIVE_AGENT_TASK_STATUSES = new Set([
  "queued",
  "running",
  "waiting_user",
  "stalled",
]);

export const TERMINAL_AGENT_TASK_STATUSES = new Set([
  "succeeded",
  "failed",
  "cancelled",
  "stopped",
]);

export const isActiveAgentRunStatus = (status: string): boolean =>
  ACTIVE_AGENT_TASK_STATUSES.has(status);

export const isTerminalAgentRunStatus = (status: string): boolean =>
  TERMINAL_AGENT_TASK_STATUSES.has(status);

export const isActiveAgentRun = (agentRun: AgentRunSummary): boolean =>
  isActiveAgentRunStatus(normalizeAgentRunStatus(agentRun.status));

export const hasMeaningfulReplayTurnContent = (
  turn: AssistantExecutionTurn,
): boolean => {
  if (turn.finalAnswer.trim()) {
    return true;
  }
  return turn.chunks.some((chunk) => {
    if (chunk.kind !== "narrative") {
      return true;
    }
    return chunk.text.trim().length > 0;
  });
};

export const selectReplayAgentRun = (
  agentRuns: AgentRunSummary[],
): AgentRunSummary | null => {
  let selected: AgentRunSummary | null = null;
  let selectedUpdatedAtMs = Number.NEGATIVE_INFINITY;
  for (const agentRun of agentRuns) {
    if (
      !agentRun.agentRunId.trim() ||
      !agentRun.sessionId.trim() ||
      (!isActiveAgentRun(agentRun) && !agentRun.unread)
    ) {
      continue;
    }
    const updatedAtMs =
      typeof agentRun.updatedAtMs === "number" && Number.isFinite(agentRun.updatedAtMs)
        ? agentRun.updatedAtMs
        : 0;
    if (!selected || updatedAtMs > selectedUpdatedAtMs) {
      selected = agentRun;
      selectedUpdatedAtMs = updatedAtMs;
    }
  }
  return selected;
};

export const buildSeenSetsFromStreamPayloads = (
  items: AgentStreamPayload[],
): StreamSeenSets => {
  const seenSessionEventIds = new Set<string>();
  let seenSessionEvent = false;
  for (const payload of items) {
    if (
      payload.type !== "runtime_event" &&
      payload.type !== "session_event" &&
      payload.type !== "error"
    ) {
      const payloadType =
        typeof payload.type === "string" && payload.type.trim()
          ? payload.type.trim()
          : "<missing>";
      throw new Error(`协议错误：不支持的 stream payload type=${payloadType}。`);
    }
    if (
      (payload.type === "runtime_event" || payload.type === "session_event") &&
      isRecord(payload.event)
    ) {
      const eventId = getSessionEventId(payload.event as SessionEvent);
      if (eventId) {
        seenSessionEventIds.add(eventId);
        seenSessionEvent = true;
      }
    }
  }
  return {
    seenSessionEventIds,
    seenSessionEvent,
  };
};

export const buildAgentRunReplayMessage = (
  agentRun: AgentRunSummary,
  items: AgentStreamPayload[],
): ChatMessage | null => {
  const isAgentRunActive = isActiveAgentRun(agentRun);
  const replayTurn = buildAssistantTurnFromStreamItems(
    items,
    "",
    typeof agentRun.completedAtMs === "number" ? agentRun.completedAtMs : agentRun.updatedAtMs,
  );
  if (!isAgentRunActive && !hasMeaningfulReplayTurnContent(replayTurn)) {
    return null;
  }
  return {
    id: `assistant-task-${agentRun.agentRunId}`,
    role: "assistant",
    turn: {
      ...replayTurn,
      isStreaming: isAgentRunActive,
      startedAtMs: agentRun.startedAtMs ?? replayTurn.startedAtMs,
      completedAtMs: isAgentRunActive
        ? undefined
        : (agentRun.completedAtMs ?? agentRun.updatedAtMs ?? replayTurn.completedAtMs),
      activity: isAgentRunActive ? DEFAULT_RUNTIME_ACTIVITY : undefined,
    },
  };
};

export const buildPendingQuestionFromSummary = (
  summary: PendingQuestionSummary | null,
): PendingQuestionRequest | null => {
  if (!summary) {
    return null;
  }
  const request = parsePendingQuestionRequest(summary.question_request);
  if (request) {
    return request;
  }
  const fallbackQuestion = isRecord(summary.question_request)
    ? summary.question_request
    : {};
  const questionText =
    typeof fallbackQuestion.question === "string"
      ? fallbackQuestion.question.trim()
      : "";
  const questionId =
    typeof summary.question_id === "string" ? summary.question_id.trim() : "";
  if (!questionId || !questionText) {
    return null;
  }
  return {
    id: questionId,
    question: questionText,
    options: [],
    multiSelect: false,
    required: true,
  };
};

export const buildRestoreTurn = (
  pendingQuestionRequest: PendingQuestionRequest | null,
): AssistantExecutionTurn | null => {
  const chunks: Array<NarrativeChunk | TaskChunk | SubagentChunk> = [];
  if (pendingQuestionRequest) {
    chunks.push({
      id: makeId("restore-question-task"),
      kind: "task",
      task: {
        id: pendingQuestionRequest.id,
        title: "等待补充信息",
        summary: pendingQuestionRequest.question,
        status: "running",
        provider: "tool",
      },
    });
  }
  if (chunks.length === 0) {
    return null;
  }
  return {
    id: makeId("assistant-restore-turn"),
    chunks,
    finalAnswer: "",
    isStreaming: false,
  };
};

export const readHydrationValue = async <T,>(
  label: string,
  task: Promise<T>,
): Promise<T> => {
  try {
    return await task;
  } catch (error) {
    throw new Error(`${label}失败：${formatExecutionError(error)}`);
  }
};

export type HydrationControl = {
  isCancelled?: () => boolean;
  yieldToUi?: () => Promise<void>;
  onStage?: (stage: string) => void;
};

const HYDRATION_MESSAGE_BATCH_SIZE = 12;
const HYDRATION_REPLAY_BATCH_SIZE = 8;

const defaultHydrationYield = (): Promise<void> => {
  if (
    typeof window !== "undefined" &&
    typeof window.requestAnimationFrame === "function"
  ) {
    return new Promise((resolve) => {
      window.requestAnimationFrame(() => resolve());
    });
  }
  return new Promise((resolve) => {
    setTimeout(resolve, 0);
  });
};

const assertHydrationNotCancelled = (control?: HydrationControl): void => {
  if (control?.isCancelled?.()) {
    throw new Error("历史恢复已取消");
  }
};

const yieldHydration = async (control?: HydrationControl): Promise<void> => {
  assertHydrationNotCancelled(control);
  await (control?.yieldToUi ?? defaultHydrationYield)();
  assertHydrationNotCancelled(control);
};

const setHydrationStage = (
  control: HydrationControl | undefined,
  stage: string,
): void => {
  assertHydrationNotCancelled(control);
  control?.onStage?.(stage);
};

export const nextReplayCursorFromPayloads = (
  agentRunId: string,
  items: readonly AgentStreamPayload[],
  fallbackCursor: number,
): number => {
  const normalizedAgentRunId = normalizeAgentRunId(agentRunId);
  const patch = deriveReplayCursorPatch(items);
  return Math.max(
    fallbackCursor,
    normalizedAgentRunId ? (patch[normalizedAgentRunId] ?? fallbackCursor) : fallbackCursor,
  );
};

export const replayAgentRunStreamFromCursor = async (
  agentRunId: string,
  startCursor: number,
): Promise<AgentRunReplaySnapshot> => {
  const normalizedAgentRunId = normalizeAgentRunId(agentRunId);
  if (!normalizedAgentRunId) {
    throw new Error("恢复任务流失败：agentRunId 为空");
  }
  if (!Number.isInteger(startCursor) || startCursor < 0) {
    throw new Error(
      `恢复任务流 ${normalizedAgentRunId} 失败：cursor 非法 ${startCursor}`,
    );
  }
  let cursor = startCursor;
  const items: AgentStreamPayload[] = [];
  for (let page = 0; page < AGENT_RUN_STREAM_REPLAY_MAX_PAGES; page += 1) {
    const requestCursor = cursor;
    const response = await readHydrationValue(
      `恢复任务流 ${normalizedAgentRunId}`,
      replayAgentRunStream({
        agentRunId: normalizedAgentRunId,
        cursor: requestCursor,
        limit: AGENT_RUN_STREAM_REPLAY_PAGE_SIZE,
      }),
    );
    const responseAgentRunId = normalizeAgentRunId(response.agentRunId);
    if (responseAgentRunId && responseAgentRunId !== normalizedAgentRunId) {
      throw new Error(
        `恢复任务流 ${normalizedAgentRunId} 失败：响应 agentRunId 不一致 ${responseAgentRunId}`,
      );
    }
    const pageItems = normalizePersistedStreamPayloads(response.items);
    if (pageItems.length === 0) {
      return {
        items,
        nextCursor: cursor,
      };
    }
    items.push(...pageItems);
    cursor = nextReplayCursorFromPayloads(
      normalizedAgentRunId,
      pageItems,
      requestCursor,
    );
    const nextCursor =
      typeof response.nextCursor === "number" ? response.nextCursor : null;
    if (nextCursor === null) {
      return {
        items,
        nextCursor: cursor,
      };
    }
    if (nextCursor <= requestCursor) {
      throw new Error(
        `恢复任务流 ${normalizedAgentRunId} 失败：cursor 未前进 ${requestCursor} -> ${nextCursor}`,
      );
    }
    cursor = nextCursor;
  }
  throw new Error(
    `恢复任务流 ${normalizedAgentRunId} 失败：超过 ${AGENT_RUN_STREAM_REPLAY_MAX_PAGES} 页`,
  );
};

export const buildAgentRunSummaryMap = (
  agentRuns: readonly AgentRunSummary[],
): Map<string, AgentRunSummary> => {
  const byAgentRunId = new Map<string, AgentRunSummary>();
  agentRuns.forEach((agentRun) => {
    const agentRunId = normalizeAgentRunId(agentRun.agentRunId);
    if (agentRunId) {
      if (byAgentRunId.has(agentRunId)) {
        throw new Error(`历史恢复失败：session projection 重复 task ${agentRunId}`);
      }
      byAgentRunId.set(agentRunId, agentRun);
    }
  });
  return byAgentRunId;
};

export const assertSessionProjection = (
  projection: SessionProjectionData,
  sessionId: string,
): void => {
  if (!projection || typeof projection !== "object") {
    throw new Error("历史恢复失败：session projection 为空");
  }
  if (projection.schemaVersion !== SESSION_PROJECTION_SCHEMA_VERSION) {
    throw new Error(
      `历史恢复失败：session projection schema 不匹配 ${projection.schemaVersion}`,
    );
  }
  if (!projection.session || typeof projection.session !== "object") {
    throw new Error("历史恢复失败：session projection 缺少 session");
  }
  const projectionSessionId =
    typeof projection.session.id === "string"
      ? projection.session.id.trim()
      : "";
  if (projectionSessionId !== sessionId) {
    throw new Error(
      `历史恢复失败：session projection sessionId 不匹配 ${projectionSessionId}`,
    );
  }
  if (!Array.isArray(projection.agentRuns)) {
    throw new Error("历史恢复失败：session projection 缺少 agentRuns");
  }
  if (!Array.isArray(projection.agentRunReplays)) {
    throw new Error("历史恢复失败：session projection 缺少 agentRunReplays");
  }
};

export const buildProjectionReplaySnapshots = (
  projection: SessionProjectionData,
  sessionId: string,
): Map<string, AgentRunReplaySnapshot> => {
  const snapshots = new Map<string, AgentRunReplaySnapshot>();
  projection.agentRunReplays.forEach((entry) => {
    const agentRunId = normalizeAgentRunId(entry.agentRunId);
    if (!agentRunId) {
      throw new Error("历史恢复失败：session projection 包含空 agentRunId");
    }
    if (snapshots.has(agentRunId)) {
      throw new Error(`历史恢复失败：session projection 重复 replay ${agentRunId}`);
    }
    const replaySessionId =
      typeof entry.sessionId === "string" ? entry.sessionId.trim() : "";
    if (replaySessionId !== sessionId) {
      throw new Error(
        `历史恢复失败：session projection replay ${agentRunId} sessionId 不匹配`,
      );
    }
    if (!Number.isInteger(entry.nextCursor) || entry.nextCursor < 0) {
      throw new Error(
        `历史恢复失败：session projection task ${agentRunId} cursor 非法`,
      );
    }
    if (!Array.isArray(entry.items)) {
      throw new Error(
        `历史恢复失败：session projection task ${agentRunId} items 非法`,
      );
    }
    snapshots.set(agentRunId, {
      items: assertProjectionStreamPayloads(agentRunId, entry.items),
      nextCursor: entry.nextCursor,
    });
  });
  return snapshots;
};

export const buildProjectionReplaySnapshotsChunked = async (
  projection: SessionProjectionData,
  sessionId: string,
  control?: HydrationControl,
): Promise<Map<string, AgentRunReplaySnapshot>> => {
  const snapshots = new Map<string, AgentRunReplaySnapshot>();
  for (let index = 0; index < projection.agentRunReplays.length; index += 1) {
    assertHydrationNotCancelled(control);
    const entry = projection.agentRunReplays[index];
    const agentRunId = normalizeAgentRunId(entry.agentRunId);
    if (!agentRunId) {
      throw new Error("历史恢复失败：session projection 包含空 agentRunId");
    }
    if (snapshots.has(agentRunId)) {
      throw new Error(`历史恢复失败：session projection 重复 replay ${agentRunId}`);
    }
    const replaySessionId =
      typeof entry.sessionId === "string" ? entry.sessionId.trim() : "";
    if (replaySessionId !== sessionId) {
      throw new Error(
        `历史恢复失败：session projection replay ${agentRunId} sessionId 不匹配`,
      );
    }
    if (!Number.isInteger(entry.nextCursor) || entry.nextCursor < 0) {
      throw new Error(
        `历史恢复失败：session projection task ${agentRunId} cursor 非法`,
      );
    }
    if (!Array.isArray(entry.items)) {
      throw new Error(
        `历史恢复失败：session projection task ${agentRunId} items 非法`,
      );
    }
    snapshots.set(agentRunId, {
      items: assertProjectionStreamPayloads(agentRunId, entry.items),
      nextCursor: entry.nextCursor,
    });
    if ((index + 1) % HYDRATION_REPLAY_BATCH_SIZE === 0) {
      await yieldHydration(control);
    }
  }
  return snapshots;
};

export const buildSessionHydrationSnapshot = async (
  sessionId: string,
  control?: HydrationControl,
): Promise<SessionHydrationSnapshot> => {
  const normalizedSessionId = sessionId.trim();
  if (!normalizedSessionId) {
    throw new Error("历史恢复失败：sessionId 为空");
  }
  assertHydrationNotCancelled(control);
  setHydrationStage(control, "fetchProjection");
  const [sessionProjection, agentState, runtimeConfig, usage] =
    await Promise.all([
      readHydrationValue(
        "读取历史会话投影",
        getSessionProjection(normalizedSessionId),
      ),
      readHydrationValue(
        "读取 agent 状态",
        getAgentState(normalizedSessionId, true),
      ),
      readHydrationValue(
        "读取运行时配置",
        getAgentRuntimeConfig(),
      ),
      readHydrationValue(
        "读取 context usage",
        getAgentContextUsage(normalizedSessionId),
      ),
    ]);

  assertHydrationNotCancelled(control);
  assertSessionProjection(sessionProjection, normalizedSessionId);
  const sessionData = sessionProjection.session;
  const agentRuns = sessionProjection.agentRuns;
  const agentRunsById = buildAgentRunSummaryMap(agentRuns);
  setHydrationStage(control, "reduceReplays");
  await yieldHydration(control);
  const replaySnapshotsByAgentRunId = await buildProjectionReplaySnapshotsChunked(
    sessionProjection,
    normalizedSessionId,
    control,
  );
  const projectedAgentRunIds = new Set<string>();
  for (const agentRun of agentRuns) {
    assertHydrationNotCancelled(control);
    const agentRunId = normalizeAgentRunId(agentRun.agentRunId);
    if (!agentRunId) {
      throw new Error("历史恢复失败：session projection tasks 包含空 agentRunId");
    }
    const agentRunSessionId =
      typeof agentRun.sessionId === "string" ? agentRun.sessionId.trim() : "";
    if (agentRunSessionId !== normalizedSessionId) {
      throw new Error(
        `历史恢复失败：session projection task ${agentRunId} sessionId 不匹配`,
      );
    }
    if (!replaySnapshotsByAgentRunId.has(agentRunId)) {
      throw new Error(
        `历史恢复失败：session projection 缺少 task replay ${agentRunId}`,
      );
    }
    projectedAgentRunIds.add(agentRunId);
  }
  await yieldHydration(control);
  for (const replayAgentRunId of replaySnapshotsByAgentRunId.keys()) {
    assertHydrationNotCancelled(control);
    if (!projectedAgentRunIds.has(replayAgentRunId)) {
      throw new Error(
        `历史恢复失败：session projection replay ${replayAgentRunId} 缺少 task summary`,
      );
    }
  }
  const replayRun = selectReplayAgentRun(agentRuns);
  const replayRunAgentRunId = replayRun ? normalizeAgentRunId(replayRun.agentRunId) : "";
  if (replayRunAgentRunId && !replaySnapshotsByAgentRunId.has(replayRunAgentRunId)) {
    throw new Error(
      `历史恢复失败：session projection 缺少 active task replay ${replayRunAgentRunId}`,
    );
  }
  const replayItemsByAgentRunId = new Map(
    Array.from(replaySnapshotsByAgentRunId.entries()).map(
      ([agentRunId, snapshot]) => [agentRunId, snapshot.items] as const,
    ),
  );
  const replayCursorsByAgentRunId = Array.from(
    replaySnapshotsByAgentRunId.entries(),
  ).reduce<SessionReplayCursors>((acc, [agentRunId, snapshot]) => {
    acc[agentRunId] = snapshot.nextCursor;
    return acc;
  }, {});

  setHydrationStage(control, "reduceMessages");
  const historyMessages = await buildHistoryMessagesChunked(
    "",
    sessionData,
    replayItemsByAgentRunId,
    agentRunsById,
    control,
  );
  await yieldHydration(control);
  const replayRunMessageId = replayRunAgentRunId
    ? findAssistantHistoryMessageIdForAgentRun(sessionData, replayRunAgentRunId)
    : null;
  const replayRunItems = replayRunAgentRunId
    ? (replayItemsByAgentRunId.get(replayRunAgentRunId) ?? [])
    : [];
  const shouldBuildDetachedReplayMessage = Boolean(
    replayRun &&
      replayRunAgentRunId &&
      !replayRunMessageId &&
      (replayRunItems.length > 0 || isActiveAgentRun(replayRun)),
  );
  const detachedReplayMessage =
    replayRun && replayRunAgentRunId && shouldBuildDetachedReplayMessage
      ? buildAgentRunReplayMessage(replayRun, replayRunItems)
      : null;
  setHydrationStage(control, "finalizeSnapshot");
  await yieldHydration(control);

  const fallbackAutoContinue = readAutoContinueAfterResumeWaitPreference();
  const resolvedAutoContinueAfterResumeWait =
    typeof runtimeConfig.autoContinueAfterResumeWait === "boolean"
      ? runtimeConfig.autoContinueAfterResumeWait
      : fallbackAutoContinue;
  const pendingQuestionRequest = buildPendingQuestionFromSummary(
    agentState.pending_questions?.[0] || null,
  );
  const restoreTurn = buildRestoreTurn(pendingQuestionRequest);
  const restoreMessageId = restoreTurn
    ? `assistant-restore-message-${normalizedSessionId}`
    : null;
  const messages = [
    ...historyMessages,
    ...(detachedReplayMessage ? [detachedReplayMessage] : []),
    ...(restoreTurn && restoreMessageId
      ? [
        {
          id: restoreMessageId,
          role: "assistant" as const,
          turn: restoreTurn,
        },
      ]
      : []),
  ];
  const activeReplay =
    replayRun &&
      replayRunAgentRunId &&
      isActiveAgentRun(replayRun)
      ? {
        messageId:
          replayRunMessageId || detachedReplayMessage?.id || "",
        agentRunId: replayRunAgentRunId,
        status: normalizeAgentRunStatus(replayRun.status),
        seedPayloads: replayRunItems,
      }
      : null;

  if (activeReplay && !activeReplay.messageId) {
    throw new Error(`恢复运行任务 ${activeReplay.agentRunId} 失败：缺少 assistant message`);
  }
  assertHydrationNotCancelled(control);

  return {
    messages,
    runtimeConfig,
    contextUsage: usage,
    resolvedAutoContinueAfterResumeWait,
    replayCursorsByAgentRunId,
    pendingQuestionRequest,
    restoreMessageId,
    activeReplay,
  };
};
