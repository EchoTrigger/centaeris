import { useCallback, useEffect, useRef } from "react";
import { observeBackgroundRuns } from "./backgroundRunObservation";
import { isNativeHostRuntime } from "../../host/hostBridge";
import type { SessionViewCacheEntry } from "../../lib/sessionViewCache";
import { waitForNextPaint } from "./chatAreaModel";
import {
  buildSessionHydrationSnapshot,
  sessionViewCacheStore,
} from "./chatRuntimeModel";
import type { SessionViewHydrationControllerOptions } from "./sessionViewHydrationPorts";
import {
  useSessionHydration,
  type SessionHydrationControl,
  type SessionHydrationPlan,
} from "./useSessionHydration";
import type { SessionViewSnapshot } from "./types";
import { useSessionViewSnapshotActions } from "./useSessionViewSnapshotActions";

export const useSessionViewHydrationController = ({
  view,
  replay,
  runtime,
  question,
  stream,
  sessionOutcome,
}: SessionViewHydrationControllerOptions) => {
  const {
    currentSessionId,
    messagesRef,
    visibleSessionIdRef,
    transcriptViewRef,
    transcriptHistoryMessageCountRef,
    setMessages,
    setTranscriptHasOlder,
    setSessionLoadError,
    setEditingUserMessageId,
    setEditingPrompt,
  } = view;
  const {
    replayCursorsByAgentRunIdRef,
    verifiedReplayAgentRunIdsRef,
    visibleActiveReplayRef,
  } = replay;
  const { resetContextUsage } = runtime;
  const { setPendingQuestion, setPendingQuestionError } = question;
  const {
    getActiveStream,
    closeActiveStream,
    setIsStreaming,
    clearStreamEventHistory,
  } = stream;
  const { pendingResolvedSessionRef, preserveResolvedSessionIdRef } =
    sessionOutcome;

  const { applyHydrationSnapshot, applySessionViewSnapshot } =
    useSessionViewSnapshotActions({
      view,
      replay,
      runtime,
      question,
      stream,
      sessionOutcome,
    });
  const refreshCachedSessionFromTranscript = useCallback(
    async (
      sessionId: string,
      cachedEntry: SessionViewCacheEntry<SessionViewSnapshot>,
      control: SessionHydrationControl,
    ) => {
      const hydrationControl = {
        isCancelled: () => !control.isLatest(),
        yieldToUi: waitForNextPaint,
        onStage: control.onStage,
      };
      control.onStage("refreshCachedSession");
      const snapshot = await buildSessionHydrationSnapshot(
        sessionId,
        hydrationControl,
        cachedEntry.snapshot.transcriptView,
      );
      if (!control.isLatest()) {
        return;
      }
      applyHydrationSnapshot(snapshot, sessionId);
    },
    [applyHydrationSnapshot],
  );

  const prepareSessionHydration = useCallback(
    (sessionId: string): SessionHydrationPlan => {
      setSessionLoadError("");
      if (
        sessionId &&
        preserveResolvedSessionIdRef.current === sessionId &&
        messagesRef.current.length > 0
      ) {
        preserveResolvedSessionIdRef.current = null;
        visibleSessionIdRef.current = sessionId;
        const activeStream = getActiveStream();
        visibleActiveReplayRef.current = activeStream?.agentRunId
          ? {
              messageId: activeStream.assistantMessageId,
              agentRunId: activeStream.agentRunId,
            }
          : visibleActiveReplayRef.current;
        setIsStreaming(Boolean(activeStream));
        return { kind: "preserved" };
      }
      closeActiveStream();
      pendingResolvedSessionRef.current = null;
      const cachedEntry = sessionId
        ? sessionViewCacheStore.get(sessionId)
        : null;
      setEditingUserMessageId(null);
      setEditingPrompt("");
      if (!sessionId) {
        visibleSessionIdRef.current = "";
        visibleActiveReplayRef.current = null;
        replayCursorsByAgentRunIdRef.current = {};
        verifiedReplayAgentRunIdsRef.current.clear();
        setPendingQuestion(null);
        setPendingQuestionError("");
        resetContextUsage("");
        setIsStreaming(false);
        transcriptViewRef.current = null;
        transcriptHistoryMessageCountRef.current = 0;
        setTranscriptHasOlder(false);
        setMessages([]);
        return { kind: "none" };
      }
      if (cachedEntry) {
        visibleSessionIdRef.current = sessionId;
        replayCursorsByAgentRunIdRef.current = {
          ...cachedEntry.replayCursorsByAgentRunId,
        };
        verifiedReplayAgentRunIdsRef.current = new Set(
          cachedEntry.verifiedReplayAgentRunIds,
        );
        transcriptViewRef.current = null;
        transcriptHistoryMessageCountRef.current = 0;
        setTranscriptHasOlder(false);
        applySessionViewSnapshot(sessionId, cachedEntry.snapshot);
        return { kind: "cached", entry: cachedEntry };
      }
      visibleSessionIdRef.current = sessionId;
      visibleActiveReplayRef.current = null;
      replayCursorsByAgentRunIdRef.current = {};
      verifiedReplayAgentRunIdsRef.current.clear();
      clearStreamEventHistory();
      setPendingQuestion(null);
      setPendingQuestionError("");
      resetContextUsage(sessionId);
      setIsStreaming(false);
      transcriptViewRef.current = null;
      transcriptHistoryMessageCountRef.current = 0;
      setTranscriptHasOlder(false);
      setMessages([]);
      return { kind: "fresh" };
    },
    [
      applySessionViewSnapshot,
      clearStreamEventHistory,
      closeActiveStream,
      getActiveStream,
      messagesRef,
      pendingResolvedSessionRef,
      preserveResolvedSessionIdRef,
      replayCursorsByAgentRunIdRef,
      resetContextUsage,
      setEditingPrompt,
      setEditingUserMessageId,
      setIsStreaming,
      setMessages,
      setPendingQuestion,
      setPendingQuestionError,
      setSessionLoadError,
      setTranscriptHasOlder,
      transcriptHistoryMessageCountRef,
      transcriptViewRef,
      verifiedReplayAgentRunIdsRef,
      visibleActiveReplayRef,
      visibleSessionIdRef,
    ],
  );

  const handleSessionHydrationError = useCallback(
    (message: string) => {
      visibleActiveReplayRef.current = null;
      replayCursorsByAgentRunIdRef.current = {};
      verifiedReplayAgentRunIdsRef.current.clear();
      setMessages([]);
      setPendingQuestion(null);
      setPendingQuestionError("");
      transcriptViewRef.current = null;
      transcriptHistoryMessageCountRef.current = 0;
      setTranscriptHasOlder(false);
      resetContextUsage(currentSessionId);
      setIsStreaming(false);
      setSessionLoadError(message);
    },
    [
      currentSessionId,
      replayCursorsByAgentRunIdRef,
      resetContextUsage,
      setIsStreaming,
      setMessages,
      setPendingQuestion,
      setPendingQuestionError,
      setSessionLoadError,
      setTranscriptHasOlder,
      transcriptHistoryMessageCountRef,
      transcriptViewRef,
      verifiedReplayAgentRunIdsRef,
      visibleActiveReplayRef,
    ],
  );

  const reconcileTerminalAgentRun = useCallback(async (sessionId: string, agentRunId: string) => {
    const expectedStream = getActiveStream();
    const ownsObservation = () => visibleSessionIdRef.current === sessionId
      && getActiveStream() === expectedStream && expectedStream?.agentRunId === agentRunId;
    if (!ownsObservation()) return false;
    const snapshot = await buildSessionHydrationSnapshot(sessionId, {
      isCancelled: () => !ownsObservation(), yieldToUi: waitForNextPaint,
    }, transcriptViewRef.current);
    if (!ownsObservation()) return false;
    closeActiveStream();
    setPendingQuestion(null);
    setPendingQuestionError("");
    applyHydrationSnapshot(snapshot, sessionId);
    setIsStreaming(Boolean(snapshot.activeReplay));
    return !snapshot.activeReplay;
  }, [applyHydrationSnapshot, closeActiveStream, getActiveStream, setIsStreaming, setPendingQuestion, setPendingQuestionError, visibleSessionIdRef, transcriptViewRef]);

  const hydration = useSessionHydration({
    currentSessionId,
    prepare: prepareSessionHydration,
    applySnapshot: applyHydrationSnapshot,
    refreshCachedSession: refreshCachedSessionFromTranscript,
    onError: handleSessionHydrationError,
  });
  const observationRef = useRef({ getActiveStream, applyHydrationSnapshot });
  observationRef.current = { getActiveStream, applyHydrationSnapshot };
  const hydratingRef = useRef(hydration.isSyncingSession);
  hydratingRef.current = hydration.isSyncingSession;
  useEffect(() => {
    if (!currentSessionId || !isNativeHostRuntime()) return;
    return observeBackgroundRuns({
      sessionId: currentSessionId,
      isKnown: (id) => observationRef.current.getActiveStream()?.agentRunId === id || verifiedReplayAgentRunIdsRef.current.has(id),
      canObserve: () => !hydratingRef.current && !observationRef.current.getActiveStream() && visibleSessionIdRef.current === currentSessionId,
      recover: async (isCurrent) => {
        const snapshot = await buildSessionHydrationSnapshot(currentSessionId, {
          isCancelled: () => !isCurrent(), yieldToUi: waitForNextPaint,
        }, transcriptViewRef.current);
        if (!isCurrent()) throw new Error("Transcript synchronization cancelled");
        observationRef.current.applyHydrationSnapshot(snapshot, currentSessionId);
        hydration.reportSyncError("");
      },
      onError: (error) => hydration.reportSyncError(error),
    });
  }, [currentSessionId, verifiedReplayAgentRunIdsRef, visibleSessionIdRef, transcriptViewRef, hydration.reportSyncError]);
  return { ...hydration, reconcileTerminalAgentRun };
};
