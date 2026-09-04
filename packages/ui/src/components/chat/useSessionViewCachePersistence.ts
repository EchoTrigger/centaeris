import { useCallback, useEffect, useRef, type RefObject } from "react";
import type { AgentContextUsageSummary } from "../../lib/chatBridge";
import type { SessionReplayCursors } from "../../lib/sessionViewCache";
import { sessionViewCacheStore } from "./chatRuntimeCore";
import type {
  ActiveStreamState,
  CachedActiveReplay,
  ChatMessage,
  PendingQuestionState,
} from "./types";

const SESSION_VIEW_CACHE_WRITE_DELAY_MS = 500;

type CurrentViewState = {
  currentSessionId: string;
  contextUsage: AgentContextUsageSummary | null;
  autoContinueAfterResumeWait: boolean | undefined;
  pendingQuestion: PendingQuestionState | null;
  pendingQuestionError: string;
};

type SessionViewCachePersistenceOptions = CurrentViewState & {
  getActiveStream: () => ActiveStreamState | null;
  messagesRef: RefObject<ChatMessage[]>;
  replayCursorsByAgentRunIdRef: RefObject<SessionReplayCursors>;
  verifiedReplayAgentRunIdsRef: RefObject<Set<string>>;
  visibleSessionIdRef: RefObject<string>;
  visibleActiveReplayRef: RefObject<CachedActiveReplay | null>;
};

export const useSessionViewCachePersistence = ({
  currentSessionId,
  contextUsage,
  autoContinueAfterResumeWait,
  pendingQuestion,
  pendingQuestionError,
  getActiveStream,
  messagesRef,
  replayCursorsByAgentRunIdRef,
  verifiedReplayAgentRunIdsRef,
  visibleSessionIdRef,
  visibleActiveReplayRef,
}: SessionViewCachePersistenceOptions) => {
  const persistTimerRef = useRef<number | null>(null);
  const currentViewStateRef = useRef<CurrentViewState>({
    currentSessionId,
    contextUsage,
    autoContinueAfterResumeWait,
    pendingQuestion,
    pendingQuestionError,
  });

  const persistVisibleSessionViewCache = useCallback(
    (sessionId: string = visibleSessionIdRef.current) => {
      const normalizedSessionId = sessionId.trim();
      if (
        !normalizedSessionId ||
        normalizedSessionId !== visibleSessionIdRef.current
      ) {
        return;
      }
      const currentViewState = currentViewStateRef.current;
      const hasViewState =
        messagesRef.current.length > 0 ||
        Boolean(currentViewState.pendingQuestion) ||
        Object.keys(replayCursorsByAgentRunIdRef.current).length > 0;
      if (!hasViewState) {
        return;
      }
      const activeStream = getActiveStream();
      const activeReplay =
        activeStream?.sessionId === normalizedSessionId &&
        activeStream.agentRunId
          ? {
              messageId: activeStream.assistantMessageId,
              agentRunId: activeStream.agentRunId,
            }
          : visibleActiveReplayRef.current;
      sessionViewCacheStore.write({
        sessionId: normalizedSessionId,
        snapshot: {
          messages: messagesRef.current,
          contextUsage: currentViewState.contextUsage,
          autoContinueAfterResumeWait:
            currentViewState.autoContinueAfterResumeWait,
          pendingQuestion: currentViewState.pendingQuestion,
          pendingQuestionError: currentViewState.pendingQuestionError,
          activeReplay,
        },
        replayCursorsByAgentRunId: replayCursorsByAgentRunIdRef.current,
        verifiedReplayAgentRunIds: Array.from(
          verifiedReplayAgentRunIdsRef.current,
        ),
      });
    },
    [
      getActiveStream,
      messagesRef,
      replayCursorsByAgentRunIdRef,
      verifiedReplayAgentRunIdsRef,
      visibleActiveReplayRef,
      visibleSessionIdRef,
    ],
  );

  const scheduleVisibleSessionViewCachePersist = useCallback(() => {
    if (persistTimerRef.current !== null) {
      return;
    }
    persistTimerRef.current = window.setTimeout(() => {
      persistTimerRef.current = null;
      persistVisibleSessionViewCache();
    }, SESSION_VIEW_CACHE_WRITE_DELAY_MS);
  }, [persistVisibleSessionViewCache]);

  useEffect(() => {
    currentViewStateRef.current = {
      currentSessionId,
      contextUsage,
      autoContinueAfterResumeWait,
      pendingQuestion,
      pendingQuestionError,
    };
    scheduleVisibleSessionViewCachePersist();
  }, [
    autoContinueAfterResumeWait,
    contextUsage,
    currentSessionId,
    pendingQuestion,
    pendingQuestionError,
    scheduleVisibleSessionViewCachePersist,
  ]);

  useEffect(
    () => () => {
      if (persistTimerRef.current !== null) {
        window.clearTimeout(persistTimerRef.current);
        persistTimerRef.current = null;
      }
    },
    [],
  );

  return {
    persistVisibleSessionViewCache,
    scheduleVisibleSessionViewCachePersist,
  };
};
