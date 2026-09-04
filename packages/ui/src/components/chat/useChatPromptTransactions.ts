import { useCallback, useEffect, useRef, type RefObject } from "react";
import { isNativeHostRuntime } from "../../host/hostBridge";
import {
  cancelAgentRun,
  createRuntimeOperationId,
  createSession,
  sendAgentInput,
  sendAgentSupplement,
  type AgentStreamPayload,
} from "../../lib/chatBridge";
import type { UiSession } from "../../types/ui";
import {
  appendNarrativeChunk,
  findEditableUserMessageIndex,
  flushDraftAnswerToNarrative,
  isDurableChatMessageId,
  prunePendingTailMessages,
  setTurnActivity,
  waitForNextPaint,
} from "./chatAreaModel";
import {
  appendGuidedSupplementChunk,
  buildPendingTurn,
  formatExecutionError,
  makeId,
  normalizeAgentRunId,
  normalizeRuntimeActivity,
} from "./chatRuntimeModel";
import type { AgentStreamConnection } from "./useAgentStreamConnection";
import type { AssistantTurnUpdateQueue } from "./useAssistantTurnUpdateQueue";
import type { useSessionRequestOwnership } from "./useSessionRequestOwnership";
import type {
  ActiveStreamState,
  CachedActiveReplay,
  ChatAreaProps,
  ChatMessage,
} from "./types";

type SetMessages = (
  updater: ChatMessage[] | ((previous: ChatMessage[]) => ChatMessage[]),
) => void;

type SessionPort = {
  currentSession: UiSession | null;
  currentSessionId: string;
  workspaceRoot?: string;
  autoContinueAfterResumeWait: boolean | undefined;
  ownership: ReturnType<typeof useSessionRequestOwnership>;
  pendingResolvedSessionRef: RefObject<UiSession | null>;
  preserveResolvedSessionIdRef: RefObject<string | null>;
  onSessionResolved: ChatAreaProps["onSessionResolved"];
  onAgentRunningChange: ChatAreaProps["onAgentRunningChange"];
};

type ViewPort = {
  inputValue: string;
  editingPrompt: string;
  messagesRef: RefObject<ChatMessage[]>;
  stoppedAssistantMessageIdsRef: RefObject<Set<string>>;
  visibleSessionIdRef: RefObject<string>;
  visibleActiveReplayRef: RefObject<CachedActiveReplay | null>;
  setInputValue: (value: string) => void;
  setEditingUserMessageId: (messageId: string | null) => void;
  setEditingPrompt: (prompt: string) => void;
  setRuntimeConfigError: (message: string) => void;
  setMessages: SetMessages;
};

type StreamPort = {
  connection: Pick<
    AgentStreamConnection,
    "getActiveStream" | "isStreaming" | "closeActiveStream"
  >;
  turnUpdates: Pick<
    AssistantTurnUpdateQueue,
    "updateAssistantTurn" | "flushAssistantTurnUpdates"
  >;
  applyDurableTurnMessageIds: (
    temporaryUserMessageId: string,
    temporaryAssistantMessageId: string,
    turnId: string | undefined,
  ) => { userMessageId: string; assistantMessageId: string };
  startStreamForAssistant: (
    assistantMessageId: string,
    sessionId: string,
    agentRunId: string,
    seedPayloads?: AgentStreamPayload[],
  ) => void;
  refreshContextUsage: (sessionId: string) => Promise<void>;
};

type QueuePort = {
  queuePrompt: (value: string, sessionId: string) => void;
  takeQueuedPromptForSession: (sessionId: string) => string;
  recoverQueuedPromptForStop: (
    sessionId: string,
    currentDraft: string,
  ) => string;
};

type NavigationPort = {
  jumpToLatest: () => void;
  resumeFollowingLatest: () => void;
};

type ChatPromptTransactionOptions = {
  session: SessionPort;
  view: ViewPort;
  stream: StreamPort;
  queue: QueuePort;
  navigation: NavigationPort;
};

type PromptMessagePair = {
  userMessage: Extract<ChatMessage, { role: "user" }>;
  assistantMessage: Extract<ChatMessage, { role: "assistant" }>;
};

const toCreatedUiSession = (
  created: Awaited<ReturnType<typeof createSession>>,
  prompt: string,
): UiSession => ({
  id: created.id,
  title: created.title || prompt,
  summary: created.lastMessage || prompt,
  updatedAt: created.updatedAt || Date.now(),
  isPinned: Boolean(created.isPinned),
  isUnread: false,
  messageCount: 0,
  cwd: created.cwd,
  sessionKind: created.sessionKind,
  parentSessionId: created.parentSessionId,
  runtimeJobId: created.runtimeJobId,
});

const buildPromptMessagePair = (prompt: string): PromptMessagePair => {
  const now = Date.now();
  return {
    userMessage: {
      id: `user-${now}`,
      role: "user",
      text: prompt,
      timestamp: now,
    },
    assistantMessage: {
      id: `assistant-${now}-${Math.random().toString(36).slice(2, 6)}`,
      role: "assistant",
      turn: buildPendingTurn(),
    },
  };
};

const buildFailedPromptMessagePair = (
  prompt: string,
  error: unknown,
): PromptMessagePair => {
  const pair = buildPromptMessagePair(prompt);
  return {
    ...pair,
    assistantMessage: {
      ...pair.assistantMessage,
      turn: {
        ...pair.assistantMessage.turn,
        isStreaming: false,
        activity: undefined,
        chunks: [
          {
            id: makeId("narrative"),
            kind: "narrative",
            text: formatExecutionError(error),
            tone: "error",
          },
        ],
      },
    },
  };
};

export const useChatPromptTransactions = ({
  session,
  view,
  stream,
  queue,
  navigation,
}: ChatPromptTransactionOptions) => {
  const {
    currentSession,
    currentSessionId,
    workspaceRoot,
    autoContinueAfterResumeWait,
    ownership,
    pendingResolvedSessionRef,
    preserveResolvedSessionIdRef,
    onSessionResolved,
    onAgentRunningChange,
  } = session;
  const {
    inputValue,
    editingPrompt,
    messagesRef,
    stoppedAssistantMessageIdsRef,
    visibleSessionIdRef,
    visibleActiveReplayRef,
    setInputValue,
    setEditingUserMessageId,
    setEditingPrompt,
    setRuntimeConfigError,
    setMessages,
  } = view;
  const {
    connection,
    turnUpdates,
    applyDurableTurnMessageIds,
    startStreamForAssistant,
    refreshContextUsage,
  } = stream;
  const { getActiveStream, isStreaming, closeActiveStream } = connection;
  const { updateAssistantTurn, flushAssistantTurnUpdates } = turnUpdates;
  const {
    adoptSession,
    captureSessionRequest,
    ownsSessionRequest,
  } = ownership;
  const {
    queuePrompt,
    takeQueuedPromptForSession,
    recoverQueuedPromptForStop,
  } = queue;
  const { jumpToLatest, resumeFollowingLatest } = navigation;
  const sendPromptRef = useRef<(queuedPrompt?: string) => Promise<void>>(
    () => Promise.resolve(),
  );
  const stopActiveAgentRunRef = useRef<() => void>(() => {});
  const submitEditedUserMessageRef = useRef<
    (messageId: string) => Promise<void>
  >(() => Promise.resolve());

  const createTargetSession = useCallback(
    async (prompt: string): Promise<UiSession> => {
      if (currentSession) {
        return currentSession;
      }
      if (!workspaceRoot) {
        throw new Error("请先选择真实工作区，再开始本地会话");
      }
      return toCreatedUiSession(
        await createSession(prompt, workspaceRoot, createRuntimeOperationId()),
        prompt,
      );
    },
    [currentSession, workspaceRoot],
  );

  const finishAcceptedPrompt = useCallback(
    (
      requestOwner: ReturnType<typeof captureSessionRequest>,
      targetSession: UiSession,
      prompt: string,
      pair: PromptMessagePair,
      response: Awaited<ReturnType<typeof sendAgentInput>>,
    ): boolean => {
      if (!ownsSessionRequest(requestOwner)) {
        return false;
      }
      const responseSessionId =
        typeof response.sessionId === "string" &&
        response.sessionId.trim().length > 0
          ? response.sessionId.trim()
          : targetSession.id;
      const responseAgentRunId =
        typeof response.agentRunId === "string" &&
        response.agentRunId.trim().length > 0
          ? response.agentRunId.trim()
          : "";
      if (!responseAgentRunId) {
        throw new Error("missing agentRunId");
      }
      const durableIds = applyDurableTurnMessageIds(
        pair.userMessage.id,
        pair.assistantMessage.id,
        response.turnId,
      );
      if (
        stoppedAssistantMessageIdsRef.current.delete(
          durableIds.assistantMessageId,
        )
      ) {
        onAgentRunningChange?.(responseSessionId, false);
        void refreshContextUsage(responseSessionId);
        return true;
      }
      pendingResolvedSessionRef.current = {
        id: responseSessionId,
        title:
          targetSession.title && targetSession.title !== "新会话"
            ? targetSession.title
            : prompt,
        summary: prompt,
        updatedAt: Date.now(),
        isPinned: targetSession.isPinned,
        isUnread: false,
        messageCount: 0,
        cwd: targetSession.cwd,
        sessionKind: targetSession.sessionKind,
        parentSessionId: targetSession.parentSessionId,
        runtimeJobId: targetSession.runtimeJobId,
      };
      startStreamForAssistant(
        durableIds.assistantMessageId,
        responseSessionId,
        responseAgentRunId,
        [],
      );
      void refreshContextUsage(responseSessionId);
      return true;
    },
    [
      applyDurableTurnMessageIds,
      onAgentRunningChange,
      ownsSessionRequest,
      pendingResolvedSessionRef,
      refreshContextUsage,
      startStreamForAssistant,
      stoppedAssistantMessageIdsRef,
    ],
  );

  const stopActiveAgentRunTransaction = useCallback(() => {
    const active = getActiveStream();
    if (!active) {
      return;
    }
    stoppedAssistantMessageIdsRef.current.add(active.assistantMessageId);
    const now = Date.now();
    updateAssistantTurn(active.assistantMessageId, (turn) => ({
      ...turn,
      isStreaming: false,
      activity: undefined,
      completedAtMs: now,
    }));
    flushAssistantTurnUpdates();
    visibleActiveReplayRef.current = null;
    onAgentRunningChange?.(active.sessionId, false);
    setInputValue(recoverQueuedPromptForStop(active.sessionId, inputValue));
    if (isNativeHostRuntime()) {
      void cancelAgentRun({
        agentRunId: active.agentRunId,
        sessionId: active.sessionId,
        reason: "user_interrupt",
      }).catch(() => {
        updateAssistantTurn(active.assistantMessageId, (turn) =>
          appendNarrativeChunk(
            turn,
            "停止请求未能写入后台任务状态。",
            "error",
          ),
        );
      });
    }
    closeActiveStream();
  }, [
    closeActiveStream,
    flushAssistantTurnUpdates,
    getActiveStream,
    inputValue,
    onAgentRunningChange,
    recoverQueuedPromptForStop,
    setInputValue,
    stoppedAssistantMessageIdsRef,
    updateAssistantTurn,
    visibleActiveReplayRef,
  ]);
  stopActiveAgentRunRef.current = stopActiveAgentRunTransaction;
  const stopActiveAgentRun = useCallback(() => {
    stopActiveAgentRunRef.current();
  }, []);

  const sendPromptAsSupplement = useCallback(
    async (prompt: string, activeStream: ActiveStreamState) => {
      const supplementAt = Date.now();
      updateAssistantTurn(activeStream.assistantMessageId, (turn) =>
        setTurnActivity(
          appendGuidedSupplementChunk(
            flushDraftAnswerToNarrative(turn),
            `optimistic-guided-supplement-${supplementAt}`,
            prompt,
            supplementAt,
          ),
          normalizeRuntimeActivity("正在处理补充输入", "thinking"),
        ),
      );
      try {
        const response = await sendAgentSupplement({
          sessionId: activeStream.sessionId,
          agentRunId: activeStream.agentRunId,
          message: prompt,
        });
        if (response.accepted !== true) {
          throw new Error(
            "_centaeris/session/supplement rejected supplement input",
          );
        }
        const expectedAgentRunId = normalizeAgentRunId(
          activeStream.agentRunId,
        );
        const responseAgentRunId = normalizeAgentRunId(response.agentRunId);
        const responseSessionId =
          typeof response.sessionId === "string"
            ? response.sessionId.trim()
            : "";
        if (
          !expectedAgentRunId ||
          responseAgentRunId !== expectedAgentRunId ||
          responseSessionId !== activeStream.sessionId
        ) {
          throw new Error(
            `_centaeris/session/supplement identity mismatch: expected=${expectedAgentRunId || "<missing>"} sessionId=${responseSessionId || "<missing>"} agentRunId=${responseAgentRunId || "<missing>"}`,
          );
        }
        void refreshContextUsage(responseSessionId);
      } catch (error) {
        updateAssistantTurn(activeStream.assistantMessageId, (turn) =>
          appendNarrativeChunk(turn, formatExecutionError(error), "error"),
        );
      }
    },
    [refreshContextUsage, updateAssistantTurn],
  );

  const sendPromptTransaction = useCallback(
    async (queuedPrompt?: string) => {
      const prompt = (queuedPrompt ?? inputValue).trim();
      if (!prompt) {
        return;
      }
      const activeStream = getActiveStream();
      if (activeStream) {
        if (queuedPrompt === undefined) {
          jumpToLatest();
        }
        queuePrompt(prompt, activeStream.sessionId);
        if (queuedPrompt === undefined) {
          setInputValue("");
        }
        return;
      }
      if (queuedPrompt === undefined) {
        resumeFollowingLatest();
      }

      let targetSession: UiSession;
      const promptOperationId = createRuntimeOperationId();
      try {
        targetSession = await createTargetSession(prompt);
      } catch (error) {
        const pair = buildFailedPromptMessagePair(prompt, error);
        setMessages((previous) => [
          ...prunePendingTailMessages(previous),
          pair.userMessage,
          pair.assistantMessage,
        ]);
        setInputValue("");
        return;
      }

      if (!currentSession) {
        adoptSession(targetSession.id);
      }
      const requestOwner = captureSessionRequest(targetSession.id);
      const pair = buildPromptMessagePair(prompt);

      visibleSessionIdRef.current = targetSession.id;
      setEditingUserMessageId(null);
      setEditingPrompt("");
      setMessages((previous) => [
        ...prunePendingTailMessages(previous),
        pair.userMessage,
        pair.assistantMessage,
      ]);
      setInputValue("");
      if (!currentSession) {
        preserveResolvedSessionIdRef.current = targetSession.id;
        onSessionResolved?.(targetSession, { activate: true });
      }

      try {
        await waitForNextPaint();
        const response = await sendAgentInput({
          operationId: promptOperationId,
          sessionId: targetSession.id,
          message: prompt,
          preferredLocale: "zh-CN",
          autoContinueAfterResumeWait,
        });
        finishAcceptedPrompt(
          requestOwner,
          targetSession,
          prompt,
          pair,
          response,
        );
      } catch (error) {
        if (!ownsSessionRequest(requestOwner)) {
          return;
        }
        onAgentRunningChange?.(targetSession.id, false);
        updateAssistantTurn(pair.assistantMessage.id, (turn) => ({
          ...appendNarrativeChunk(
            turn,
            formatExecutionError(error),
            "error",
          ),
          isStreaming: false,
          activity: undefined,
        }));
        closeActiveStream();
      }
    },
    [
      adoptSession,
      autoContinueAfterResumeWait,
      captureSessionRequest,
      closeActiveStream,
      createTargetSession,
      currentSession,
      finishAcceptedPrompt,
      getActiveStream,
      inputValue,
      jumpToLatest,
      onAgentRunningChange,
      onSessionResolved,
      ownsSessionRequest,
      preserveResolvedSessionIdRef,
      queuePrompt,
      resumeFollowingLatest,
      setEditingPrompt,
      setEditingUserMessageId,
      setInputValue,
      setMessages,
      updateAssistantTurn,
      visibleSessionIdRef,
    ],
  );
  sendPromptRef.current = sendPromptTransaction;
  const sendPrompt = useCallback(
    (queuedPrompt?: string) => sendPromptRef.current(queuedPrompt),
    [],
  );

  useEffect(() => {
    if (isStreaming || getActiveStream()) {
      return;
    }
    const prompt = takeQueuedPromptForSession(currentSessionId);
    if (!prompt) {
      return;
    }
    void sendPromptRef.current(prompt);
  }, [
    currentSessionId,
    getActiveStream,
    isStreaming,
    takeQueuedPromptForSession,
  ]);

  const submitEditedUserMessageTransaction = useCallback(
    async (messageId: string) => {
      const prompt = editingPrompt.trim();
      if (!prompt || isStreaming || getActiveStream()) {
        return;
      }
      const currentMessages = messagesRef.current;
      const editableIndex = findEditableUserMessageIndex(currentMessages);
      if (
        editableIndex < 0 ||
        currentMessages[editableIndex]?.id !== messageId ||
        !isDurableChatMessageId("user", messageId)
      ) {
        return;
      }
      const expectedTailMessage = currentMessages.at(-1);
      if (
        !expectedTailMessage ||
        (expectedTailMessage.role === "assistant" &&
          !isDurableChatMessageId("assistant", expectedTailMessage.id))
      ) {
        return;
      }
      const expectedTailMessageId = expectedTailMessage.id;

      let targetSession: UiSession;
      const promptOperationId = createRuntimeOperationId();
      try {
        targetSession = await createTargetSession(prompt);
      } catch (error) {
        const pair = buildFailedPromptMessagePair(prompt, error);
          setMessages((previous) => {
            const index = previous.findIndex((item) => item.id === messageId);
            if (index < 0) {
              return [
                ...prunePendingTailMessages(previous),
                pair.userMessage,
                pair.assistantMessage,
              ];
            }
            return [
              ...previous.slice(0, index),
              pair.userMessage,
              pair.assistantMessage,
            ];
          });
          setEditingUserMessageId(null);
          setEditingPrompt("");
          return;
      }

      if (!currentSession) {
        adoptSession(targetSession.id);
      }
      const requestOwner = captureSessionRequest(targetSession.id);
      const pair = buildPromptMessagePair(prompt);

      setMessages((previous) => {
        const index = previous.findIndex((item) => item.id === messageId);
        if (index < 0) {
          return [
            ...prunePendingTailMessages(previous),
            pair.userMessage,
            pair.assistantMessage,
          ];
        }
        return [
          ...previous.slice(0, index),
          pair.userMessage,
          pair.assistantMessage,
        ];
      });
      setEditingUserMessageId(null);
      setEditingPrompt("");
      if (!currentSession) {
        preserveResolvedSessionIdRef.current = targetSession.id;
        onSessionResolved?.(targetSession, { activate: true });
      }

      let inputAccepted = false;
      try {
        await waitForNextPaint();
        const response = await sendAgentInput({
          operationId: promptOperationId,
          sessionId: targetSession.id,
          message: prompt,
          preferredLocale: "zh-CN",
          autoContinueAfterResumeWait,
          tailPolicy: "rewriteLastUser",
          rewriteTargetMessageId: messageId,
          rewriteExpectedTailMessageId: expectedTailMessageId,
        });
        inputAccepted = true;
        finishAcceptedPrompt(
          requestOwner,
          targetSession,
          prompt,
          pair,
          response,
        );
      } catch (error) {
        if (!ownsSessionRequest(requestOwner)) {
          return;
        }
        onAgentRunningChange?.(targetSession.id, false);
        if (!inputAccepted) {
          closeActiveStream();
          setMessages(currentMessages);
          setEditingUserMessageId(messageId);
          setEditingPrompt(prompt);
          setRuntimeConfigError(
            `编辑失败：${formatExecutionError(error)}`,
          );
          return;
        }
        updateAssistantTurn(pair.assistantMessage.id, (turn) => ({
          ...appendNarrativeChunk(
            turn,
            formatExecutionError(error),
            "error",
          ),
          isStreaming: false,
          activity: undefined,
        }));
        closeActiveStream();
      }
    },
    [
      adoptSession,
      autoContinueAfterResumeWait,
      captureSessionRequest,
      closeActiveStream,
      createTargetSession,
      currentSession,
      editingPrompt,
      finishAcceptedPrompt,
      getActiveStream,
      isStreaming,
      messagesRef,
      onAgentRunningChange,
      onSessionResolved,
      ownsSessionRequest,
      preserveResolvedSessionIdRef,
      setEditingPrompt,
      setEditingUserMessageId,
      setMessages,
      setRuntimeConfigError,
      updateAssistantTurn,
    ],
  );
  submitEditedUserMessageRef.current = submitEditedUserMessageTransaction;
  const submitEditedUserMessage = useCallback(
    (messageId: string) => submitEditedUserMessageRef.current(messageId),
    [],
  );

  return {
    sendPrompt,
    sendPromptAsSupplement,
    stopActiveAgentRun,
    submitEditedUserMessage,
  };
};
