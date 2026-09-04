import {
  useCallback,
  type Dispatch,
  type RefObject,
  type SetStateAction,
} from "react";
import {
  getSession,
  listAgentRuns,
  type AgentContextUsageSummary,
  type AgentRuntimeConfig,
  type AgentStreamPayload,
  type AgentRunSummary,
} from "../../lib/chatBridge";
import {
  decideSessionViewCacheReplay,
  mergeReplayCursors,
  type SessionReplayCursors,
  type SessionViewCacheEntry,
} from "../../lib/sessionViewCache";
import type { UiSession } from "../../types/ui";
import {
  findAssistantMessageIdForAgentRunInView,
  setTurnActivity,
  waitForNextPaint,
} from "./chatAreaModel";
import {
  AUTO_CONTINUE_AFTER_RESUME_WAIT_KEY,
  RUNTIME_ACTIVITY_BY_PROCESS_STATE,
  buildSessionHydrationSnapshot,
  collectSessionVisibleMessageIds,
  isActiveAgentRun,
  normalizeAgentRunId,
  readHydrationValue,
  replayAgentRunStreamFromCursor,
  selectReplayAgentRun,
  sessionViewCacheStore,
} from "./chatRuntimeModel";
import {
  useSessionHydration,
  type SessionHydrationControl,
  type SessionHydrationPlan,
} from "./useSessionHydration";
import type { AssistantTurnUpdateQueue } from "./useAssistantTurnUpdateQueue";
import type {
  ActiveStreamState,
  CachedActiveReplay,
  ChatMessage,
  PendingQuestionState,
  SessionHydrationSnapshot,
  SessionViewSnapshot,
} from "./types";

const HYDRATION_DELTA_PAYLOAD_BATCH_SIZE = 24;

type SetMessages = (
  updater: ChatMessage[] | ((previous: ChatMessage[]) => ChatMessage[]),
) => void;

type HydrationViewPort = {
  currentSessionId: string;
  messagesRef: RefObject<ChatMessage[]>;
  visibleSessionIdRef: RefObject<string>;
  setMessages: SetMessages;
  setSessionLoadError: Dispatch<SetStateAction<string>>;
  setEditingUserMessageId: Dispatch<SetStateAction<string | null>>;
  setEditingPrompt: Dispatch<SetStateAction<string>>;
};

type HydrationReplayPort = {
  replayCursorsByAgentRunIdRef: RefObject<SessionReplayCursors>;
  verifiedReplayAgentRunIdsRef: RefObject<Set<string>>;
  visibleActiveReplayRef: RefObject<CachedActiveReplay | null>;
  persistVisibleSessionViewCache: (sessionId?: string) => void;
};

type HydrationRuntimePort = {
  setAutoContinueAfterResumeWait: Dispatch<
    SetStateAction<boolean | undefined>
  >;
  applyGlobalRuntimeConfig: (config: AgentRuntimeConfig) => void;
  applyContextUsage: (
    sessionId: string,
    contextUsage: AgentContextUsageSummary | null,
  ) => void;
  resetContextUsage: (sessionId: string) => void;
};

type HydrationQuestionPort = {
  setPendingQuestion: Dispatch<SetStateAction<PendingQuestionState | null>>;
  setPendingQuestionError: Dispatch<SetStateAction<string>>;
};

type HydrationStreamPort = {
  getActiveStream: () => ActiveStreamState | null;
  closeActiveStream: () => void;
  setIsStreaming: (isStreaming: boolean) => void;
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
  updateAssistantTurn: AssistantTurnUpdateQueue["updateAssistantTurn"];
  flushAssistantTurnUpdates: AssistantTurnUpdateQueue["flushAssistantTurnUpdates"];
};

type HydrationSessionOutcomePort = {
  pendingResolvedSessionRef: RefObject<UiSession | null>;
  preserveResolvedSessionIdRef: RefObject<string | null>;
};

type SessionViewHydrationControllerOptions = {
  view: HydrationViewPort;
  replay: HydrationReplayPort;
  runtime: HydrationRuntimePort;
  question: HydrationQuestionPort;
  stream: HydrationStreamPort;
  sessionOutcome: HydrationSessionOutcomePort;
};

const cachedSnapshotHasToolProcess = (
  snapshot: SessionViewSnapshot,
): boolean =>
  snapshot.messages.some((message) => {
    if (message.role !== "assistant") {
      return false;
    }
    return message.turn.chunks.some(
      (chunk) => chunk.kind === "task" || chunk.kind === "subagent",
    );
  });

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
    setMessages,
    setSessionLoadError,
    setEditingUserMessageId,
    setEditingPrompt,
  } = view;
  const {
    replayCursorsByAgentRunIdRef,
    verifiedReplayAgentRunIdsRef,
    visibleActiveReplayRef,
    persistVisibleSessionViewCache,
  } = replay;
  const {
    setAutoContinueAfterResumeWait,
    applyGlobalRuntimeConfig,
    applyContextUsage,
    resetContextUsage,
  } = runtime;
  const { setPendingQuestion, setPendingQuestionError } = question;
  const {
    getActiveStream,
    closeActiveStream,
    setIsStreaming,
    processStreamPayload,
    startStreamForAssistant,
    clearStreamEventHistory,
    updateAssistantTurn,
    flushAssistantTurnUpdates,
  } = stream;
  const { pendingResolvedSessionRef, preserveResolvedSessionIdRef } =
    sessionOutcome;

  const applySessionViewSnapshot = useCallback(
    (sessionId: string, snapshot: SessionViewSnapshot) => {
      visibleSessionIdRef.current = sessionId;
      visibleActiveReplayRef.current = snapshot.activeReplay;
      setAutoContinueAfterResumeWait(snapshot.autoContinueAfterResumeWait);
      applyContextUsage(sessionId, snapshot.contextUsage);
      setPendingQuestion(snapshot.pendingQuestion);
      setPendingQuestionError(snapshot.pendingQuestionError);
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
      visibleActiveReplayRef,
      visibleSessionIdRef,
    ],
  );

  const markAgentRunTerminalInView = useCallback(
    (agentRun: AgentRunSummary) => {
      if (isActiveAgentRun(agentRun)) {
        return;
      }
      const messageId = findAssistantMessageIdForAgentRunInView(
        messagesRef.current,
        agentRun.agentRunId,
      );
      if (!messageId) {
        return;
      }
      if (visibleActiveReplayRef.current?.agentRunId === agentRun.agentRunId) {
        visibleActiveReplayRef.current = null;
      }
      updateAssistantTurn(messageId, (turn) => ({
        ...turn,
        isStreaming: false,
        activity: undefined,
        completedAtMs:
          agentRun.completedAtMs ??
          agentRun.updatedAtMs ??
          turn.completedAtMs ??
          Date.now(),
      }));
    },
    [messagesRef, updateAssistantTurn, visibleActiveReplayRef],
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
      startStreamForAssistant,
      verifiedReplayAgentRunIdsRef,
      visibleActiveReplayRef,
      visibleSessionIdRef,
    ],
  );

  const refreshCachedSessionFromDurableLog = useCallback(
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
      const [sessionData, taskResponse] = await Promise.all([
        readHydrationValue("读取历史会话", getSession(sessionId)),
        readHydrationValue(
          "读取任务列表",
          listAgentRuns({
            sessionId,
            includeTerminal: true,
          }),
        ),
      ]);
      if (!control.isLatest()) {
        return;
      }
      const agentRuns = Array.isArray(taskResponse.agentRuns)
        ? taskResponse.agentRuns
        : [];
      const durableAgentRunIds = agentRuns.map((agentRun) => {
        const agentRunId = normalizeAgentRunId(agentRun.agentRunId);
        if (!agentRunId) {
          throw new Error("历史恢复失败：任务列表包含空 agentRunId");
        }
        return agentRunId;
      });
      const replayDecision =
        durableAgentRunIds.length > 0 &&
        !cachedSnapshotHasToolProcess(cachedEntry.snapshot)
          ? {
              kind: "fullReplay" as const,
              reason: "cached_snapshot_missing_tool_process",
            }
          : decideSessionViewCacheReplay({
              durableMessageIds: collectSessionVisibleMessageIds(sessionData),
              durableStreamAgentRunIds: durableAgentRunIds,
              cachedMessageIds: messagesRef.current.map(
                (message) => message.id,
              ),
              cachedReplayCursorsByAgentRunId:
                cachedEntry.replayCursorsByAgentRunId,
              cachedVerifiedReplayAgentRunIds:
                cachedEntry.verifiedReplayAgentRunIds,
            });
      if (replayDecision.kind === "fullReplay") {
        console.info("会话缓存需要从 durable log 完整恢复", {
          sessionId,
          reason: replayDecision.reason,
        });
        const snapshot = await buildSessionHydrationSnapshot(
          sessionId,
          hydrationControl,
        );
        if (!control.isLatest()) {
          return;
        }
        applyHydrationSnapshot(snapshot, sessionId);
        return;
      }
      const replayRun = selectReplayAgentRun(agentRuns);
      const agentRunIds = new Set(
        Object.keys(cachedEntry.replayCursorsByAgentRunId).map(
          normalizeAgentRunId,
        ),
      );
      durableAgentRunIds.forEach((agentRunId) => {
        agentRunIds.add(agentRunId);
      });
      if (replayRun) {
        const replayRunAgentRunId = normalizeAgentRunId(replayRun.agentRunId);
        if (replayRunAgentRunId) {
          agentRunIds.add(replayRunAgentRunId);
        }
      }
      const normalizedAgentRunIds = Array.from(agentRunIds).filter(Boolean);
      control.onStage("fetchDeltaReplays");
      const deltaEntries = await Promise.all(
        normalizedAgentRunIds.map(async (agentRunId) => {
          const cursor =
            cachedEntry.replayCursorsByAgentRunId[agentRunId] ?? 0;
          const snapshot = await replayAgentRunStreamFromCursor(
            agentRunId,
            cursor,
          );
          return [agentRunId, snapshot] as const;
        }),
      );
      if (!control.isLatest()) {
        return;
      }
      control.onStage("applyDeltaReplays");
      let cursorPatch: SessionReplayCursors = {};
      let appliedDeltaPayloads = 0;
      for (const [agentRunId, snapshot] of deltaEntries) {
        if (!control.isLatest()) {
          return;
        }
        cursorPatch = mergeReplayCursors(cursorPatch, {
          [agentRunId]: snapshot.nextCursor,
        });
        if (snapshot.items.length === 0) {
          continue;
        }
        const messageId = findAssistantMessageIdForAgentRunInView(
          messagesRef.current,
          agentRunId,
        );
        if (!messageId) {
          throw new Error(
            `缓存会话 ${sessionId} 增量恢复失败：task ${agentRunId} 缺少 assistant message`,
          );
        }
        for (const payload of snapshot.items) {
          if (!control.isLatest()) {
            return;
          }
          processStreamPayload(messageId, payload);
          appliedDeltaPayloads += 1;
          if (
            appliedDeltaPayloads % HYDRATION_DELTA_PAYLOAD_BATCH_SIZE ===
            0
          ) {
            flushAssistantTurnUpdates();
            await waitForNextPaint();
            if (!control.isLatest()) {
              return;
            }
          }
        }
      }
      flushAssistantTurnUpdates();
      replayCursorsByAgentRunIdRef.current = mergeReplayCursors(
        replayCursorsByAgentRunIdRef.current,
        cursorPatch,
      );
      sessionViewCacheStore.patchReplayCursors(sessionId, cursorPatch);
      for (const agentRun of agentRuns) {
        markAgentRunTerminalInView(agentRun);
      }
      if (!replayRun || !isActiveAgentRun(replayRun)) {
        visibleActiveReplayRef.current = null;
      }
      if (replayRun && isActiveAgentRun(replayRun)) {
        const agentRunId = normalizeAgentRunId(replayRun.agentRunId);
        const messageId = findAssistantMessageIdForAgentRunInView(
          messagesRef.current,
          agentRunId,
        );
        if (!messageId) {
          throw new Error(
            `缓存会话 ${sessionId} attach 失败：task ${agentRunId} 缺少 assistant message`,
          );
        }
        const seedPayloads =
          deltaEntries.find(
            ([entryAgentRunId]) => entryAgentRunId === agentRunId,
          )?.[1].items ?? [];
        startStreamForAssistant(
          messageId,
          sessionId,
          agentRunId,
          seedPayloads,
        );
      }
      persistVisibleSessionViewCache(sessionId);
    },
    [
      applyHydrationSnapshot,
      flushAssistantTurnUpdates,
      markAgentRunTerminalInView,
      messagesRef,
      persistVisibleSessionViewCache,
      processStreamPayload,
      replayCursorsByAgentRunIdRef,
      startStreamForAssistant,
      visibleActiveReplayRef,
    ],
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
        setIsStreaming(false);
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
      verifiedReplayAgentRunIdsRef,
      visibleActiveReplayRef,
    ],
  );

  return useSessionHydration({
    currentSessionId,
    prepare: prepareSessionHydration,
    applySnapshot: applyHydrationSnapshot,
    refreshCachedSession: refreshCachedSessionFromDurableLog,
    onError: handleSessionHydrationError,
  });
};
