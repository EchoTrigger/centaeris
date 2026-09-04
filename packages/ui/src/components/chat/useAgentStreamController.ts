import {
  useCallback,
  useRef,
  type Dispatch,
  type RefObject,
  type SetStateAction,
} from "react";
import {
  openAgentStream,
  type AgentStreamPayload,
} from "../../lib/chatBridge";
import type { UiSession } from "../../types/ui";
import {
  appendNarrativeChunk,
  setTurnActivity,
} from "./chatAreaModel";
import {
  RUNTIME_ACTIVITY_BY_PROCESS_STATE,
} from "./chatRuntimeCore";
import { buildSeenSetsFromStreamPayloads } from "./chatRuntimeModel";
import type { AgentStreamConnection } from "./useAgentStreamConnection";
import { useAgentStreamEventLifecycle } from "./useAgentStreamEventLifecycle";
import { useAgentStreamTerminalLifecycle } from "./useAgentStreamTerminalLifecycle";
import type { AssistantTurnUpdateQueue } from "./useAssistantTurnUpdateQueue";
import type {
  CachedActiveReplay,
  ChatAreaProps,
  PendingQuestionState,
  RuntimeActivity,
  StreamSeenSets,
} from "./types";

export type AgentStreamContextPort = {
  markContextCompacting: (sessionId: string, isCompacting: boolean) => void;
  refreshContextUsage: (sessionId: string) => Promise<void>;
};

export type AgentStreamQuestionPort = {
  setPendingQuestion: Dispatch<SetStateAction<PendingQuestionState | null>>;
  setPendingQuestionError: Dispatch<SetStateAction<string>>;
};

export type AgentStreamSessionOutcomePort = {
  pendingResolvedSessionRef: RefObject<UiSession | null>;
  preserveResolvedSessionIdRef: RefObject<string | null>;
  onSessionResolved: ChatAreaProps["onSessionResolved"];
  onAgentRunningChange: ChatAreaProps["onAgentRunningChange"];
  onSessionCompleted: ChatAreaProps["onSessionCompleted"];
};

export type AgentStreamReplayPort = {
  rememberReplayPayloads: (payloads: readonly AgentStreamPayload[]) => void;
  visibleActiveReplayRef: RefObject<CachedActiveReplay | null>;
};

type AgentStreamControllerOptions = {
  connection: AgentStreamConnection;
  turnUpdates: AssistantTurnUpdateQueue;
  context: AgentStreamContextPort;
  question: AgentStreamQuestionPort;
  sessionOutcome: AgentStreamSessionOutcomePort;
  replay: AgentStreamReplayPort;
};

export type AgentStreamController = {
  processStreamPayload: (
    assistantMessageId: string,
    payload: AgentStreamPayload,
  ) => void;
  startStreamForAssistant: (
    assistantMessageId: string,
    sessionId: string,
    agentRunId: string,
    seedPayloads?: AgentStreamPayload[],
  ) => void;
  clearStreamEventHistory: () => void;
};

export const useAgentStreamController = ({
  connection,
  turnUpdates,
  context,
  question,
  sessionOutcome,
  replay,
}: AgentStreamControllerOptions): AgentStreamController => {
  const {
    getActiveStream,
    isActiveStream,
    setActiveStream,
    markStreamOpen,
    closeActiveStream,
    closeStreamForMessage,
  } = connection;
  const {
    updateAssistantTurn,
    appendAssistantTextDelta,
    flushAssistantTurnUpdates,
  } = turnUpdates;
  const { markContextCompacting, refreshContextUsage } = context;
  const { setPendingQuestion, setPendingQuestionError } = question;
  const {
    pendingResolvedSessionRef,
    preserveResolvedSessionIdRef,
    onSessionResolved,
    onAgentRunningChange,
    onSessionCompleted,
  } = sessionOutcome;
  const { rememberReplayPayloads, visibleActiveReplayRef } = replay;
  const streamSeenByMessageIdRef = useRef<Map<string, StreamSeenSets>>(
    new Map(),
  );

  const finishAssistantStreamWithError = useCallback(
    (
      assistantMessageId: string,
      message: string,
      activity?: RuntimeActivity,
      turnId?: string,
    ) => {
      const active = getActiveStream();
      const completedAtMs = Date.now();
      updateAssistantTurn(assistantMessageId, (turn) => {
        const withActivity = activity ? setTurnActivity(turn, activity) : turn;
        const withError = appendNarrativeChunk(
          withActivity,
          message,
          "error",
          turnId,
        );
        return {
          ...withError,
          isStreaming: false,
          completedAtMs,
        };
      });
      flushAssistantTurnUpdates();
      if (active?.assistantMessageId === assistantMessageId) {
        visibleActiveReplayRef.current = null;
        onAgentRunningChange?.(active.sessionId, false);
      }
      closeStreamForMessage(assistantMessageId);
    },
    [
      closeStreamForMessage,
      flushAssistantTurnUpdates,
      getActiveStream,
      onAgentRunningChange,
      updateAssistantTurn,
      visibleActiveReplayRef,
    ],
  );

  const handleTerminalSessionEvent = useAgentStreamTerminalLifecycle({
    connection: { getActiveStream, closeStreamForMessage },
    turnUpdates: {
      updateAssistantTurn,
      flushAssistantTurnUpdates,
      finishAssistantStreamWithError,
    },
    sessionOutcome: {
      pendingResolvedSessionRef,
      preserveResolvedSessionIdRef,
      visibleActiveReplayRef,
      onSessionResolved,
      onAgentRunningChange,
      onSessionCompleted,
    },
    context: { refreshContextUsage },
  });

  const { processStreamPayload } = useAgentStreamEventLifecycle({
    connection: { getActiveStream },
    turnUpdates: {
      finishAssistantStreamWithError,
      appendAssistantTextDelta,
      updateAssistantTurn,
    },
    context: { markContextCompacting, refreshContextUsage },
    question: { setPendingQuestion, setPendingQuestionError },
    replay: { streamSeenByMessageIdRef, rememberReplayPayloads },
    handleTerminalSessionEvent,
  });

  const startStreamForAssistant = useCallback(
    (
      assistantMessageId: string,
      sessionId: string,
      agentRunId: string,
      seedPayloads: AgentStreamPayload[] = [],
    ) => {
      updateAssistantTurn(assistantMessageId, (turn) =>
        turn.agentRunId === agentRunId ? turn : { ...turn, agentRunId },
      );
      if (
        getActiveStream()?.assistantMessageId === assistantMessageId &&
        getActiveStream()?.agentRunId === agentRunId
      ) {
        return;
      }
      closeActiveStream();
      onAgentRunningChange?.(sessionId, true);
      const replaySeen = buildSeenSetsFromStreamPayloads(seedPayloads);
      streamSeenByMessageIdRef.current.set(assistantMessageId, replaySeen);
      visibleActiveReplayRef.current = agentRunId
        ? {
            messageId: assistantMessageId,
            agentRunId,
          }
        : null;
      const stream = openAgentStream(
        agentRunId,
        (payload) => {
          if (!isActiveStream({ assistantMessageId, agentRunId })) {
            return;
          }
          processStreamPayload(assistantMessageId, payload);
        },
        (error) => {
          if (
            !getActiveStream() ||
            getActiveStream()?.assistantMessageId !== assistantMessageId
          ) {
            return;
          }
          const detail =
            error instanceof Error && error.message.trim()
              ? `连接中断：${error.message.trim()}`
              : "连接中断，请重试。";
          finishAssistantStreamWithError(
            assistantMessageId,
            detail,
            RUNTIME_ACTIVITY_BY_PROCESS_STATE.provider_interrupted,
          );
        },
        () => {
          markStreamOpen({ assistantMessageId, agentRunId });
        },
      );

      setActiveStream({
        sessionId,
        agentRunId,
        assistantMessageId,
        seenSessionEvent: replaySeen.seenSessionEvent,
        seenSessionEventIds: replaySeen.seenSessionEventIds,
        close: stream.close,
      });
    },
    [
      closeActiveStream,
      finishAssistantStreamWithError,
      getActiveStream,
      isActiveStream,
      markStreamOpen,
      onAgentRunningChange,
      processStreamPayload,
      setActiveStream,
      updateAssistantTurn,
      visibleActiveReplayRef,
    ],
  );

  const clearStreamEventHistory = useCallback(() => {
    streamSeenByMessageIdRef.current.clear();
  }, []);

  return {
    processStreamPayload,
    startStreamForAssistant,
    clearStreamEventHistory,
  };
};
