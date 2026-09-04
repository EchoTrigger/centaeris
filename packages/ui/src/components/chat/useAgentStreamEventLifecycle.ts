import {
  useCallback,
  type Dispatch,
  type RefObject,
  type SetStateAction,
} from "react";
import type {
  AgentStreamPayload,
  SessionEvent,
} from "../../lib/chatBridge";
import { appendNarrativeChunk, setTurnActivity } from "./chatAreaModel";
import { runtimeEasterEgg } from "./chatRuntimeCore";
import {
  applySessionEventToAssistantTurn,
  formatRuntimeModelError,
  getEventTurnId,
  getSessionEventId,
  isRecord,
  mapPreparingToolNameToActivity,
  mapProcessStateToActivity,
  mapRuntimePayloadToActivity,
  normalizeRuntimeActivity,
  parsePendingQuestionRequest,
} from "./chatRuntimeModel";
import type {
  ActiveStreamState,
  AssistantExecutionTurn,
  PendingQuestionState,
  RuntimeActivity,
  StreamSeenSets,
} from "./types";

type UpdateAssistantTurn = (
  messageId: string,
  updater: (turn: AssistantExecutionTurn) => AssistantExecutionTurn,
) => void;

type FinishAssistantStreamWithError = (
  assistantMessageId: string,
  message: string,
  activity?: RuntimeActivity,
  turnId?: string,
) => void;

type HandleTerminalSessionEvent = (
  assistantMessageId: string,
  payloadAgentRunId: unknown,
  event: SessionEvent,
) => boolean;

type UseAgentStreamEventLifecycleOptions = {
  connection: {
    getActiveStream: () => ActiveStreamState | null;
  };
  turnUpdates: {
    finishAssistantStreamWithError: FinishAssistantStreamWithError;
    appendAssistantTextDelta: (messageId: string, delta: string) => void;
    updateAssistantTurn: UpdateAssistantTurn;
  };
  context: {
    markContextCompacting: (sessionId: string, isCompacting: boolean) => void;
    refreshContextUsage: (sessionId: string) => Promise<void>;
  };
  question: {
    setPendingQuestion: Dispatch<SetStateAction<PendingQuestionState | null>>;
    setPendingQuestionError: Dispatch<SetStateAction<string>>;
  };
  replay: {
    streamSeenByMessageIdRef: RefObject<Map<string, StreamSeenSets>>;
    rememberReplayPayloads: (payloads: readonly AgentStreamPayload[]) => void;
  };
  handleTerminalSessionEvent: HandleTerminalSessionEvent;
};

export const useAgentStreamEventLifecycle = ({
  connection,
  turnUpdates,
  context,
  question,
  replay,
  handleTerminalSessionEvent,
}: UseAgentStreamEventLifecycleOptions) => {
  const { getActiveStream } = connection;
  const {
    finishAssistantStreamWithError,
    appendAssistantTextDelta,
    updateAssistantTurn,
  } = turnUpdates;
  const { markContextCompacting, refreshContextUsage } = context;
  const { setPendingQuestion, setPendingQuestionError } = question;
  const { streamSeenByMessageIdRef, rememberReplayPayloads } = replay;
  const ensureStreamSeenSets = useCallback(
    (assistantMessageId: string): StreamSeenSets => {
      const active = getActiveStream();
      if (active?.assistantMessageId === assistantMessageId) {
        return active;
      }
      const existing = streamSeenByMessageIdRef.current.get(assistantMessageId);
      if (existing) {
        return existing;
      }
      const created: StreamSeenSets = {
        seenSessionEventIds: new Set<string>(),
        seenSessionEvent: false,
      };
      streamSeenByMessageIdRef.current.set(assistantMessageId, created);
      return created;
    },
    [getActiveStream, streamSeenByMessageIdRef],
  );

  const applySessionEventRuntimeFact = useCallback(
    (assistantMessageId: string, event: SessionEvent) => {
      if (
        typeof event.visibility === "string" &&
        event.visibility.toLowerCase() !== "user"
      ) {
        return;
      }

      const payload = isRecord(event.payload) ? event.payload : {};
      const activityPayload = {
        ...payload,
        processState: event.processState ?? payload.processState,
      };
      const statusRaw =
        typeof event.status === "string"
          ? event.status.toLowerCase()
          : "running";
      const eventTurnId = getEventTurnId(event);

      if (
        event.type === "ToolCallPreparing" ||
        event.type === "ToolCallReady"
      ) {
        const toolName =
          typeof event.toolName === "string" ? event.toolName.trim() : "";
        if (!toolName) {
          finishAssistantStreamWithError(
            assistantMessageId,
            `协议错误：${event.type} 缺少 toolName。`,
            normalizeRuntimeActivity("协议错误", "summarizing"),
            eventTurnId,
          );
          return;
        }
        updateAssistantTurn(assistantMessageId, (turn) =>
          setTurnActivity(turn, mapPreparingToolNameToActivity(toolName)),
        );
        return;
      }

      if (event.type === "ModelRequestStart" || event.type === "ModelStatus") {
        if (event.type === "ModelRequestStart") {
          const purpose =
            typeof payload.purpose === "string" ? payload.purpose : "";
          const contextTokenEstimate = payload.contextTokenEstimate;
          if (
            !["main", "compaction"].includes(purpose) ||
            typeof contextTokenEstimate !== "number" ||
            !Number.isInteger(contextTokenEstimate) ||
            contextTokenEstimate < 0
          ) {
            finishAssistantStreamWithError(
              assistantMessageId,
              "协议错误：ModelRequestStart 缺少有效的 purpose/contextTokenEstimate。",
              normalizeRuntimeActivity("协议错误", "summarizing"),
              eventTurnId,
            );
            return;
          }
          const eventSessionId =
            typeof event.sessionId === "string" && event.sessionId.trim()
              ? event.sessionId.trim()
              : getActiveStream()?.sessionId || "";
          markContextCompacting(eventSessionId, purpose === "compaction");
          void refreshContextUsage(eventSessionId);
        }
        const activity = mapRuntimePayloadToActivity(
          activityPayload,
          event.type === "ModelRequestStart"
            ? payload.purpose === "compaction"
              ? "compressing"
              : "thinking"
            : undefined,
        );
        if (activity) {
          updateAssistantTurn(assistantMessageId, (turn) =>
            setTurnActivity(turn, activity),
          );
        }
        return;
      }

      if (event.type === "ModelTextDelta") {
        const delta = typeof payload.delta === "string" ? payload.delta : "";
        appendAssistantTextDelta(assistantMessageId, delta);
        return;
      }

      if (event.type === "Status") {
        const text =
          typeof payload.message === "string" ? payload.message.trim() : "";
        const isAgentProcessTitle =
          payload.stage === "model_process_summary" && statusRaw !== "failed";
        if (!text) {
          return;
        }
        updateAssistantTurn(assistantMessageId, (turn) => {
          let nextTurn = applySessionEventToAssistantTurn(turn, event);
          const activity = mapRuntimePayloadToActivity(
            activityPayload,
            undefined,
            isAgentProcessTitle ? text || undefined : undefined,
          );
          if (isAgentProcessTitle) {
            nextTurn = setTurnActivity(
              nextTurn,
              activity?.processState &&
                runtimeEasterEgg(turn.agentRunId, activity.processState)
                ? activity
                : null,
            );
          } else if (activity) {
            nextTurn = setTurnActivity(nextTurn, activity);
          }
          if (text && statusRaw === "failed") {
            nextTurn = appendNarrativeChunk(
              nextTurn,
              text,
              "error",
              eventTurnId,
            );
          }
          return nextTurn;
        });
        return;
      }

      if (event.type === "RuntimeError") {
        finishAssistantStreamWithError(
          assistantMessageId,
          formatRuntimeModelError(payload),
          undefined,
          eventTurnId,
        );
        return;
      }

      if (event.type === "QuestionRequired") {
        const text =
          typeof payload.message === "string"
            ? payload.message.trim()
            : "需要补充信息后继续执行。";
        const questionRequest = parsePendingQuestionRequest(
          payload.questionRequest,
        );
        if (questionRequest) {
          setPendingQuestion({
            assistantMessageId,
            request: questionRequest,
            selectedOptions: [],
            answerText: "",
            submitting: false,
          });
          setPendingQuestionError("");
        }
        updateAssistantTurn(assistantMessageId, (turn) =>
          appendNarrativeChunk(
            setTurnActivity(
              turn,
              mapRuntimePayloadToActivity(activityPayload),
            ),
            text,
            "normal",
            eventTurnId,
          ),
        );
        return;
      }

      updateAssistantTurn(assistantMessageId, (turn) => {
        let nextTurn = applySessionEventToAssistantTurn(turn, event);
        if (event.processState) {
          nextTurn = setTurnActivity(
            nextTurn,
            mapProcessStateToActivity(event.processState),
          );
        }
        return nextTurn;
      });
    },
    [
      appendAssistantTextDelta,
      finishAssistantStreamWithError,
      getActiveStream,
      markContextCompacting,
      refreshContextUsage,
      setPendingQuestion,
      setPendingQuestionError,
      updateAssistantTurn,
    ],
  );

  const processStreamPayload = useCallback(
    (assistantMessageId: string, payload: AgentStreamPayload) => {
      rememberReplayPayloads([payload]);
      if (payload.type === "runtime_event" || payload.type === "session_event") {
        if (isRecord(payload.event)) {
          const event = payload.event as SessionEvent;
          const seen = ensureStreamSeenSets(assistantMessageId);
          const eventId = getSessionEventId(event);
          if (eventId) {
            if (seen.seenSessionEventIds.has(eventId)) {
              return;
            }
            seen.seenSessionEventIds.add(eventId);
          }
          seen.seenSessionEvent = true;
          if (
            payload.type === "session_event" &&
            handleTerminalSessionEvent(
              assistantMessageId,
              payload.agentRunId,
              event,
            )
          ) {
            return;
          }
          applySessionEventRuntimeFact(assistantMessageId, event);
        }
        return;
      }

      if (payload.type === "error") {
        const message =
          typeof payload.message === "string"
            ? payload.message
            : "处理消息时发生错误。";
        finishAssistantStreamWithError(
          assistantMessageId,
          message,
          normalizeRuntimeActivity("处理异常", "summarizing"),
        );
        return;
      }

      const payloadType =
        typeof payload.type === "string" && payload.type.trim()
          ? payload.type.trim()
          : "<missing>";
      finishAssistantStreamWithError(
        assistantMessageId,
        `协议错误：不支持的 stream payload type=${payloadType}。`,
        normalizeRuntimeActivity("协议错误", "summarizing"),
      );
    },
    [
      applySessionEventRuntimeFact,
      ensureStreamSeenSets,
      finishAssistantStreamWithError,
      handleTerminalSessionEvent,
      rememberReplayPayloads,
    ],
  );

  return { processStreamPayload };
};
