import { useCallback, useRef } from "react";
import { getAgentContextUsage, getAgentRuntimeConfig } from "../../lib/chatBridge";
import {
  AUTO_CONTINUE_AFTER_RESUME_WAIT_KEY,
  formatExecutionError,
  readAutoContinueAfterResumeWaitPreference,
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
    captureSessionRequest,
    ownsSessionRequest,
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
    reportRuntimeConfigError,
  } = runtime;
  const { setPendingQuestion, setPendingQuestionError } = question;
  const { setIsStreaming, startStreamForAssistant } = stream;

  const metadataRequestIdRef = useRef(0);
  const refreshMetadata = useCallback((sessionId: string) => {
    const requestId = ++metadataRequestIdRef.current;
    const owner = captureSessionRequest(sessionId);
    const isCurrent = () => requestId === metadataRequestIdRef.current
      && ownsSessionRequest(owner) && visibleSessionIdRef.current === sessionId;
    // Optional reads publish independently and never gate or replace the body.
    void getAgentRuntimeConfig().then(config => {
      if (!isCurrent()) return;
      applyGlobalRuntimeConfig(config);
      const preference = typeof config.autoContinueAfterResumeWait === "boolean"
        ? config.autoContinueAfterResumeWait : readAutoContinueAfterResumeWaitPreference();
      setAutoContinueAfterResumeWait(preference);
      if (typeof preference === "boolean") {
        window.localStorage.setItem(AUTO_CONTINUE_AFTER_RESUME_WAIT_KEY, String(preference));
      }
    }).catch(error => { if (isCurrent()) reportRuntimeConfigError(formatExecutionError(error)); });
    void getAgentContextUsage(sessionId).then(usage => {
      if (isCurrent()) applyContextUsage(sessionId, usage);
    }).catch(() => {
      // Keep the last canonical usage boundary, as with live usage refreshes.
    });
  }, [applyContextUsage, applyGlobalRuntimeConfig, captureSessionRequest, ownsSessionRequest,
    reportRuntimeConfigError, setAutoContinueAfterResumeWait, visibleSessionIdRef]);

  const applySessionViewSnapshot = useCallback(
    (sessionId: string, snapshot: SessionViewSnapshot) => {
      visibleSessionIdRef.current = sessionId;
      visibleActiveReplayRef.current = snapshot.activeReplay;
      setAutoContinueAfterResumeWait(snapshot.autoContinueAfterResumeWait);
      applyContextUsage(sessionId, snapshot.contextUsage);
      setPendingQuestion(snapshot.pendingQuestion);
      setPendingQuestionError(snapshot.pendingQuestionError);
      transcriptViewRef.current = snapshot.transcriptView ?? null;
      transcriptHistoryMessageCountRef.current = snapshot.transcriptHistoryMessageCount ?? 0;
      setTranscriptHasOlder(snapshot.transcriptView?.hasOlder ?? false);
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
      const cached = sessionViewCacheStore.get(sessionId);
      const hydratedMessages = snapshot.messages;
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
      const transcriptView = snapshot.transcriptView ?? (snapshot.transcriptPage
        ? DesktopTranscriptView.open(snapshot.transcriptPage)
        : null);
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
      setPendingQuestion(pendingQuestion);
      sessionViewCacheStore.write({
        sessionId,
        snapshot: {
          messages: hydratedMessages,
          transcriptView,
          transcriptHistoryMessageCount: snapshot.transcriptHistoryMessageCount,
          contextUsage: cached?.snapshot.contextUsage ?? null,
          autoContinueAfterResumeWait: cached?.snapshot.autoContinueAfterResumeWait
            ?? readAutoContinueAfterResumeWaitPreference(),
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
      refreshMetadata(sessionId);
    },
    [
      refreshMetadata,
      replayCursorsByAgentRunIdRef,
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
