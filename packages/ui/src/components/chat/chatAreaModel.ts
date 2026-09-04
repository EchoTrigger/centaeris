import {
  hasNarrativeChunk,
  isPersistedAssistantErrorStatus,
  makeId,
  normalizeAgentRunId,
} from "./chatRuntimeModel";
import type {
  AssistantExecutionTurn,
  ChatMessage,
  EventWaterfall,
  NarrativeProjectionMeta,
  RuntimeActivity,
  SubagentResult,
  TaskResult,
} from "./types";

export const MESSAGE_FOLLOW_BOTTOM_THRESHOLD_PX = 64;

export const isNearScrollBottom = (element: HTMLElement): boolean =>
  element.scrollHeight - element.scrollTop - element.clientHeight <=
  MESSAGE_FOLLOW_BOTTOM_THRESHOLD_PX;

export const recoverQueuedPromptAfterStop = (
  queuedPrompt: string,
  currentDraft: string,
): string =>
  [queuedPrompt.trim(), currentDraft.trim()].filter(Boolean).join("\n\n");

export const toOutputLineCount = (value: string | undefined): number | undefined => {
  if (!value) {
    return undefined;
  }
  const rows = value
    .split(/\r?\n/)
    .map((item) => item.trim())
    .filter((item) => item.length > 0);
  return rows.length;
};

export const findTaskById = (
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

export const findSubagentById = (
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

export const appendNarrativeChunk = (
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
        id: makeId("narrative"),
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

export const flushDraftAnswerToNarrative = (
  turn: AssistantExecutionTurn,
  turnId?: string,
): AssistantExecutionTurn => {
  const draft = turn.finalAnswer.trim();
  if (!draft) {
    return turn;
  }
  return appendNarrativeChunk(
    {
      ...turn,
      finalAnswer: "",
    },
    draft,
    "normal",
    turnId,
  );
};

export const setTurnActivity = (
  turn: AssistantExecutionTurn,
  activity: RuntimeActivity | null,
): AssistantExecutionTurn => {
  if (!activity) {
    return turn.activity === null ? turn : { ...turn, activity: null };
  }
  if (
    turn.activity?.kind === activity.kind &&
    turn.activity.label === activity.label &&
    turn.activity.processState === activity.processState
  ) {
    return turn;
  }
  return {
    ...turn,
    activity,
  };
};

export const upsertTaskChunk = (
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
      id: makeId("task"),
      kind: "task",
      task,
    });
  }
  return {
    ...turn,
    chunks,
  };
};

export const waitForNextPaint = (): Promise<void> => {
  if (
    typeof window === "undefined" ||
    typeof window.requestAnimationFrame !== "function"
  ) {
    return Promise.resolve();
  }
  return new Promise((resolve) => {
    window.requestAnimationFrame(() => resolve());
  });
};

const assistantTurnHasError = (turn: AssistantExecutionTurn): boolean =>
  turn.chunks.some(
    (chunk) => chunk.kind === "narrative" && chunk.tone === "error",
  );

const assistantTurnErrorText = (turn: AssistantExecutionTurn): string => {
  const parts = [turn.finalAnswer.trim()];
  turn.chunks.forEach((chunk) => {
    if (chunk.kind === "narrative" && chunk.tone === "error") {
      parts.push(chunk.text.trim());
    }
  });
  return parts.filter(Boolean).join("\n");
};

const hasPositiveNumber = (value: unknown): boolean =>
  typeof value === "number" && Number.isFinite(value) && value > 0;

const taskHasFileSideEffect = (task: TaskResult): boolean => {
  return (task.operations ?? []).some((operation) => {
    return (
      operation.toolName === "write" ||
      operation.toolName === "edit" ||
      hasPositiveNumber(operation.added) ||
      hasPositiveNumber(operation.removed)
    );
  });
};

const assistantTurnHasFileSideEffect = (turn: AssistantExecutionTurn): boolean =>
  turn.chunks.some(
    (chunk) => chunk.kind === "task" && taskHasFileSideEffect(chunk.task),
  );

const isAbandonableAssistantFailureText = (value: string): boolean => {
  const normalized = value.trim().toLowerCase();
  if (!normalized) {
    return false;
  }
  return [
    "model_client_error(",
    "model compaction request failed(",
    "task mode router model call failed(",
    "provider_response_interrupted",
    "provider_busy_or_rate_limited",
    "provider_unavailable",
    "provider api_base is required",
    "unknown model provider",
    "requires api key",
    "missing auth env var",
    "auth_failed",
    "model_unavailable",
    "invalid_request",
    "read sse chunk failed",
    "read http body failed",
    "error decoding response body",
    "response body",
    "operation timed out",
    "deadline has elapsed",
    "timed out",
    "timeout",
    "network",
    "模型服务响应中断",
    "模型服务端故障",
    "模型服务排队中",
    "模型服务鉴权失败",
  ].some((needle) => normalized.includes(needle));
};

const isAbandonableFailedAssistantTail = (message: ChatMessage): boolean =>
  message.role === "assistant" &&
  !message.turn.isStreaming &&
  !assistantTurnHasFileSideEffect(message.turn) &&
  isAbandonableAssistantFailureText(assistantTurnErrorText(message.turn));

const assistantTurnHasMeaningfulOutput = (
  turn: AssistantExecutionTurn,
): boolean => {
  if (turn.finalAnswer.trim()) {
    return true;
  }
  return turn.chunks.some((chunk) => {
    if (chunk.kind !== "narrative") {
      return true;
    }
    return chunk.tone !== "error" && chunk.text.trim().length > 0;
  });
};

const isEmptyFailedAssistantMessage = (message: ChatMessage): boolean =>
  message.role === "assistant" &&
  !message.turn.isStreaming &&
  !assistantTurnHasMeaningfulOutput(message.turn);

const isBlockingAssistantTailForUserEdit = (message: ChatMessage): boolean =>
  message.role === "assistant" &&
  (message.turn.isStreaming ||
    (isEmptyFailedAssistantMessage(message) &&
      !isAbandonableFailedAssistantTail(message)) ||
    ((isPersistedAssistantErrorStatus(
      (message.status ?? "").trim().toLowerCase(),
    ) ||
      assistantTurnHasError(message.turn)) &&
      !isAbandonableFailedAssistantTail(message)));

export const findEditableUserMessageIndex = (items: ChatMessage[]): number => {
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index];
    if (item.role === "user") {
      return index;
    }
    if (isBlockingAssistantTailForUserEdit(item)) {
      return -1;
    }
  }
  return -1;
};

export const isDurableChatMessageId = (
  role: "user" | "assistant",
  messageId: string | undefined,
): boolean =>
  typeof messageId === "string" && messageId.startsWith(`msg:${role}:turn-`);

export const findAssistantMessageIdForAgentRunInView = (
  items: ChatMessage[],
  agentRunId: string,
): string | null => {
  const normalizedAgentRunId = normalizeAgentRunId(agentRunId);
  if (!normalizedAgentRunId) {
    return null;
  }
  for (const item of items) {
    if (item.role !== "assistant") {
      continue;
    }
    if (item.id === `assistant-task-${normalizedAgentRunId}`) {
      return item.id;
    }
  }
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index];
    if (item.role === "assistant" && item.turn.isStreaming) {
      return item.id;
    }
  }
  return null;
};

export const prunePendingTailMessages = (items: ChatMessage[]): ChatMessage[] => {
  const next = [...items];
  for (; ;) {
    const beforeLength = next.length;
    while (next.at(-1)?.role === "user") {
      next.pop();
    }
    const last = next.at(-1);
    const previous = next.at(-2);
    if (
      last &&
      previous &&
      isEmptyFailedAssistantMessage(last) &&
      previous.role === "user"
    ) {
      next.pop();
      next.pop();
    }
    if (next.length === beforeLength) {
      return next;
    }
  }
};
