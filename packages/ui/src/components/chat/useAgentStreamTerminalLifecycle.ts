import { useCallback, type RefObject } from "react";
import type { SessionEvent } from "../../lib/chatBridge";
import type { UiSession } from "../../types/ui";
import {
  getTerminalSessionEventStatus,
  normalizeAgentRunId,
  normalizeRuntimeActivity,
} from "./chatRuntimeModel";
import type {
  ActiveStreamState,
  AssistantExecutionTurn,
  CachedActiveReplay,
  ChatAreaProps,
  RuntimeActivity,
} from "./types";

type FinishAssistantStreamWithError = (
  assistantMessageId: string,
  message: string,
  activity?: RuntimeActivity,
  turnId?: string,
) => void;

type UpdateAssistantTurn = (
  messageId: string,
  updater: (turn: AssistantExecutionTurn) => AssistantExecutionTurn,
) => void;

type AgentStreamTerminalLifecycleOptions = {
  connection: {
    getActiveStream: () => ActiveStreamState | null;
    closeStreamForMessage: (assistantMessageId: string) => void;
  };
  turnUpdates: {
    finishAssistantStreamWithError: FinishAssistantStreamWithError;
    updateAssistantTurn: UpdateAssistantTurn;
    flushAssistantTurnUpdates: () => void;
  };
  sessionOutcome: {
    pendingResolvedSessionRef: RefObject<UiSession | null>;
    preserveResolvedSessionIdRef: RefObject<string | null>;
    visibleActiveReplayRef: RefObject<CachedActiveReplay | null>;
    onSessionResolved: ChatAreaProps["onSessionResolved"];
    onAgentRunningChange: ChatAreaProps["onAgentRunningChange"];
    onSessionCompleted: ChatAreaProps["onSessionCompleted"];
  };
  context: {
    refreshContextUsage: (sessionId: string) => Promise<void>;
  };
};

export const useAgentStreamTerminalLifecycle = ({
  connection,
  turnUpdates,
  sessionOutcome,
  context,
}: AgentStreamTerminalLifecycleOptions) => {
  const { getActiveStream, closeStreamForMessage } = connection;
  const {
    finishAssistantStreamWithError,
    updateAssistantTurn,
    flushAssistantTurnUpdates,
  } = turnUpdates;
  const {
    pendingResolvedSessionRef,
    preserveResolvedSessionIdRef,
    visibleActiveReplayRef,
    onSessionResolved,
    onAgentRunningChange,
    onSessionCompleted,
  } = sessionOutcome;
  const { refreshContextUsage } = context;

  return useCallback(
    (
      assistantMessageId: string,
      payloadAgentRunId: unknown,
      event: SessionEvent,
    ): boolean => {
      try {
        if (!getTerminalSessionEventStatus(event)) {
          return false;
        }
      } catch (error) {
        finishAssistantStreamWithError(
          assistantMessageId,
          error instanceof Error ? error.message : String(error),
          normalizeRuntimeActivity("协议错误", "summarizing"),
        );
        return true;
      }

      const active = getActiveStream();
      if (
        active?.agentRunId &&
        normalizeAgentRunId(payloadAgentRunId) !== active.agentRunId
      ) {
        finishAssistantStreamWithError(
          assistantMessageId,
          "协议错误：终态 session_event 的 agentRunId 与活动 AgentRun 不匹配。",
          normalizeRuntimeActivity("协议错误", "summarizing"),
        );
        return true;
      }
      if (active?.assistantMessageId === assistantMessageId) {
        visibleActiveReplayRef.current = null;
      }
      updateAssistantTurn(assistantMessageId, (turn) => ({
        ...turn,
        isStreaming: false,
        activity: undefined,
        completedAtMs:
          typeof event.at === "number" && Number.isFinite(event.at)
            ? event.at
            : Date.now(),
      }));
      flushAssistantTurnUpdates();
      const resolvedSession = pendingResolvedSessionRef.current;
      if (resolvedSession && active?.sessionId === resolvedSession.id) {
        pendingResolvedSessionRef.current = null;
        preserveResolvedSessionIdRef.current = resolvedSession.id;
        onSessionResolved?.(resolvedSession, { activate: false });
      }
      const sessionId =
        (typeof event.sessionId === "string" && event.sessionId.trim()) ||
        active?.sessionId ||
        "";
      if (active?.assistantMessageId === assistantMessageId && sessionId) {
        onAgentRunningChange?.(sessionId, false);
        onSessionCompleted?.(sessionId);
        void refreshContextUsage(sessionId);
      }
      closeStreamForMessage(assistantMessageId);
      return true;
    },
    [
      closeStreamForMessage,
      finishAssistantStreamWithError,
      flushAssistantTurnUpdates,
      getActiveStream,
      onAgentRunningChange,
      onSessionCompleted,
      onSessionResolved,
      pendingResolvedSessionRef,
      preserveResolvedSessionIdRef,
      refreshContextUsage,
      updateAssistantTurn,
      visibleActiveReplayRef,
    ],
  );
};
