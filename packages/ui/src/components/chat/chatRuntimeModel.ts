import { t } from "../../i18n";
import {
  getAgentContextUsage,
  getAgentRunLiveSnapshot,
  getAgentRuntimeConfig,
  getAgentState,
  listAgentRuns,
  type AgentStreamPayload,
  type AgentRunSummary,
  type SessionData,
  type SessionEvent,
  type PendingQuestionSummary,
  type PersistedChatMessage,
} from "../../lib/chatBridge";
import type {
  AssistantExecutionTurn,
  ChatMessage,
  NarrativeChunk,
  PendingQuestionRequest,
  SessionHydrationSnapshot,
  StreamSeenSets,
  SubagentChunk,
  TaskChunk,
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
  getSessionEventId,
  normalizePersistedContent,
} from "./chatTranscriptRestore";
import {
  DesktopTranscriptView,
  loadTranscriptPage,
} from "./transcriptPaging";
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
    const text = content || (hasImage ? t("chatRuntimeModel.imageMessage") : "");
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
    throw new Error(t("chatRuntimeModel.historyMessageValueIsMissingAgentrunid", { value1: messageId }));
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
      throw new Error(t("chatRuntimeModel.protocolErrorUnsupportedStreamPayloadTypeValue", { value1: payloadType }));
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
        title: t("chatPendingPanels.waitingForMoreInformation"),
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
    throw new Error(t("chatRuntimeModel.valueFailedValue", { value1: label, value2: formatExecutionError(error) }));
  }
};

export type HydrationControl = {
  isCancelled?: () => boolean;
  yieldToUi?: () => Promise<void>;
  onStage?: (stage: string) => void;
};

const HYDRATION_MESSAGE_BATCH_SIZE = 12;

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
    throw new Error(t("chatRuntimeModel.historyRecoveryCancelled"));
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

export const buildSessionHydrationSnapshot = async (
  sessionId: string,
  control?: HydrationControl,
): Promise<SessionHydrationSnapshot> => {
  const normalizedSessionId = sessionId.trim();
  if (!normalizedSessionId) {
    throw new Error(t("chatRuntimeModel.historyRecoveryFailedSessionidIsEmpty"));
  }
  assertHydrationNotCancelled(control);
  setHydrationStage(control, "fetchProjection");
  const [transcriptPage, taskResponse, agentState, runtimeConfig, usage] =
    await Promise.all([
      readHydrationValue(
        t("chatRuntimeModel.loadingHistoryConversationProjection"),
        loadTranscriptPage({ sessionId: normalizedSessionId }),
      ),
      readHydrationValue(
        t("chatRuntimeModel.loadingAgentState"),
        listAgentRuns({
          sessionId: normalizedSessionId,
          includeTerminal: false,
        }),
      ),
      readHydrationValue(
        t("chatRuntimeModel.loadingAgentState"),
        getAgentState(normalizedSessionId, true),
      ),
      readHydrationValue(
        t("chatRuntimeModel.loadingRuntimeConfiguration"),
        getAgentRuntimeConfig(),
      ),
      readHydrationValue(
        t("chatRuntimeModel.loadingContextUsage"),
        getAgentContextUsage(normalizedSessionId),
      ),
    ]);

  assertHydrationNotCancelled(control);
  const agentRuns = Array.isArray(taskResponse.agentRuns)
    ? taskResponse.agentRuns
    : [];
  for (const agentRun of agentRuns) {
    if (agentRun.sessionId.trim() !== normalizedSessionId) {
      throw new Error(
        t("chatRuntimeModel.historyRecoveryFailedSessionProjectionTaskValueSessionidMismatch", {
          value1: normalizeAgentRunId(agentRun.agentRunId),
        }),
      );
    }
  }
  const replayRun = selectReplayAgentRun(agentRuns);
  const replayRunAgentRunId = replayRun
    ? normalizeAgentRunId(replayRun.agentRunId)
    : "";
  let seedPayloads: AgentStreamPayload[] = [];
  if (replayRun && replayRunAgentRunId && isActiveAgentRun(replayRun)) {
    setHydrationStage(control, "reduceReplays");
    const replay = await readHydrationValue(
      t("chatRuntimeModel.loadingAgentState"),
      getAgentRunLiveSnapshot({
        agentRunId: replayRunAgentRunId,
      }),
    );
    assertHydrationNotCancelled(control);
    if (replay.agentRunId.trim() !== replayRunAgentRunId) {
      throw new Error(
        t("chatRuntimeModel.historyRecoveryFailedSessionProjectionReplayValueSessionidMismatch", {
          value1: replayRunAgentRunId,
        }),
      );
    }
    seedPayloads = replay.liveSnapshot ? [replay.liveSnapshot] : [];
  }

  setHydrationStage(control, "reduceMessages");
  await yieldHydration(control);
  const transcriptView = DesktopTranscriptView.open(transcriptPage);
  const historyMessages = transcriptView.materializeMessages(
    Boolean(replayRun && isActiveAgentRun(replayRun)),
  );
  const activeMessage =
    replayRun && replayRunAgentRunId && isActiveAgentRun(replayRun)
      ? buildAgentRunReplayMessage(replayRun, seedPayloads)
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
    ...(activeMessage ? [activeMessage] : []),
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
    replayRun && replayRunAgentRunId && activeMessage
      ? {
          messageId: activeMessage.id,
          agentRunId: replayRunAgentRunId,
          status: normalizeAgentRunStatus(replayRun.status),
          seedPayloads,
        }
      : null;
  assertHydrationNotCancelled(control);

  return {
    messages,
    transcriptPage,
    transcriptHistoryMessageCount: historyMessages.length,
    runtimeConfig,
    contextUsage: usage,
    resolvedAutoContinueAfterResumeWait,
    replayCursorsByAgentRunId: {},
    pendingQuestionRequest,
    restoreMessageId,
    activeReplay,
  };
};
