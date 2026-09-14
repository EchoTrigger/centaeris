import { useCallback } from "react";
import { setTurnActivity } from "./chatAreaModel";
import {
  AUTO_CONTINUE_AFTER_RESUME_WAIT_KEY,
  RUNTIME_ACTIVITY_BY_PROCESS_STATE,
  sessionViewCacheStore,
} from "./chatRuntimeModel";
import { DesktopTranscriptView } from "./transcriptPaging";
import type { SessionViewHydrationControllerOptions } from "./sessionViewHydrationPorts";
import type {
  SessionHydrationSnapshot,
  SessionViewSnapshot,
} from "./types";

export const useSessionViewSnapshotActions = ({
  view,
  replay,
  runtime,
  question,
  stream,
}: SessionViewHydrationControllerOptions) => {
  const {
    visibleSessionIdRef,
    transcriptViewRef,
    transcriptHistoryMessageCountRef,
    setMessages,
    setTranscriptHasOlder,
    setSessionLoadError,
  } = view;
  const {
    replayCursorsByAgentRunIdRef,
    verifiedReplayAgentRunIdsRef,
    visibleActiveReplayRef,
  } = replay;
  const {
    setAutoContinueAfterResumeWait,
    applyGlobalRuntimeConfig,
    applyContextUsage,
  } = runtime;
  const { setPendingQuestion, setPendingQuestionError } = question;
  const { setIsStreaming, startStreamForAssistant } = stream;

  const applySessionViewSnapshot = useCallback(
    (sessionId: string, snapshot: SessionViewSnapshot) => {
      visibleSessionIdRef.current = sessionId;
      visibleActiveReplayRef.current = snapshot.activeReplay;
      setAutoContinueAfterResumeWait(snapshot.autoContinueAfterResumeWait);
      applyContextUsage(sessionId, snapshot.contextUsage);
      setPendingQuestion(snapshot.pendingQuestion);
      setPendingQuestionError(snapshot.pendingQuestionError);
      transcriptViewRef.current = null;
      transcriptHistoryMessageCountRef.current = 0;
      setTranscriptHasOlder(false);
      setMessages(snapshot.messages);
      setIsStreaming(Boolean(snapshot.activeReplay));
    },
    [
      applyContextUsage,
      setAutoContinueAfterResumeWait,
      setIsStreaming,
      setMessages,
      setPendingQuestion,
      setPendingQuestionError,
      setTranscriptHasOlder,
      transcriptHistoryMessageCountRef,
      transcriptViewRef,
      visibleActiveReplayRef,
      visibleSessionIdRef,
    ],
  );

  const applyHydrationSnapshot = useCallback(
    (snapshot: SessionHydrationSnapshot, sessionId: string) => {
      const hydratedMessages =
        snapshot.contextUsage?.isCompacting && snapshot.activeReplay
          ? snapshot.messages.map((message) =>
              message.role === "assistant" &&
              message.id === snapshot.activeReplay?.messageId
                ? {
                    ...message,
                    turn: setTurnActivity(
                      message.turn,
                      RUNTIME_ACTIVITY_BY_PROCESS_STATE.compressing,
                    ),
                  }
                : message,
            )
          : snapshot.messages;
      setSessionLoadError("");
      visibleSessionIdRef.current = sessionId;
      visibleActiveReplayRef.current = snapshot.activeReplay
        ? {
            messageId: snapshot.activeReplay.messageId,
            agentRunId: snapshot.activeReplay.agentRunId,
          }
        : null;
      replayCursorsByAgentRunIdRef.current = {
        ...snapshot.replayCursorsByAgentRunId,
      };
      verifiedReplayAgentRunIdsRef.current = new Set(
        Object.keys(snapshot.replayCursorsByAgentRunId),
      );
      setAutoContinueAfterResumeWait(
        snapshot.resolvedAutoContinueAfterResumeWait,
      );
      if (
        typeof snapshot.resolvedAutoContinueAfterResumeWait === "boolean" &&
        typeof window !== "undefined" &&
        window.localStorage
      ) {
        window.localStorage.setItem(
          AUTO_CONTINUE_AFTER_RESUME_WAIT_KEY,
          snapshot.resolvedAutoContinueAfterResumeWait ? "true" : "false",
        );
      }
      applyGlobalRuntimeConfig(snapshot.runtimeConfig);
      applyContextUsage(sessionId, snapshot.contextUsage);
      const transcriptView = snapshot.transcriptPage
        ? DesktopTranscriptView.open(snapshot.transcriptPage)
        : null;
      transcriptViewRef.current = transcriptView;
      transcriptHistoryMessageCountRef.current =
        snapshot.transcriptHistoryMessageCount ?? 0;
      setTranscriptHasOlder(transcriptView?.hasOlder ?? false);
      setMessages(hydratedMessages);
      const pendingQuestion =
        snapshot.pendingQuestionRequest && snapshot.restoreMessageId
          ? {
              assistantMessageId: snapshot.restoreMessageId,
              request: snapshot.pendingQuestionRequest,
              selectedOptions: [],
              answerText: "",
              submitting: false,
            }
          : null;
      if (pendingQuestion) {
        setPendingQuestion(pendingQuestion);
      }
      sessionViewCacheStore.write({
        sessionId,
        snapshot: {
          messages: hydratedMessages,
          contextUsage: snapshot.contextUsage,
          autoContinueAfterResumeWait:
            snapshot.resolvedAutoContinueAfterResumeWait,
          pendingQuestion,
          pendingQuestionError: "",
          activeReplay: snapshot.activeReplay
            ? {
                messageId: snapshot.activeReplay.messageId,
                agentRunId: snapshot.activeReplay.agentRunId,
              }
            : null,
        },
        replayCursorsByAgentRunId: snapshot.replayCursorsByAgentRunId,
        verifiedReplayAgentRunIds: Object.keys(
          snapshot.replayCursorsByAgentRunId,
        ),
      });
      if (snapshot.activeReplay) {
        startStreamForAssistant(
          snapshot.activeReplay.messageId,
          sessionId,
          snapshot.activeReplay.agentRunId,
          snapshot.activeReplay.seedPayloads,
        );
      }
    },
    [
      applyContextUsage,
      applyGlobalRuntimeConfig,
      replayCursorsByAgentRunIdRef,
      setAutoContinueAfterResumeWait,
      setMessages,
      setPendingQuestion,
      setSessionLoadError,
      setTranscriptHasOlder,
      startStreamForAssistant,
      transcriptHistoryMessageCountRef,
      transcriptViewRef,
      verifiedReplayAgentRunIdsRef,
      visibleActiveReplayRef,
      visibleSessionIdRef,
    ],
  );

  return { applyHydrationSnapshot, applySessionViewSnapshot } as const;
};
