import { t } from "../../i18n";
import {
  type CSSProperties,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  CloudUpload,
  ChevronDown,
  CornerDownLeft,
  FileText,
  GitBranch,
  HardDrive,
  Pencil,
} from "lucide-react";
import { GitHubMarkIcon } from "../icons/GitHubMarkIcon";
import type { UiSession } from "../../types/ui";
import {
  type AgentStreamPayload,
} from "../../lib/chatBridge";
import {
  deriveReplayCursorPatch,
  mergeReplayCursors,
  type SessionReplayCursors,
} from "../../lib/sessionViewCache";
import { Button } from "../ui/button";
import { Tooltip } from "../ui/tooltip";
import { ChatComposer } from "./ChatComposer";
import { PendingQuestionPanel } from "./ChatPendingPanels";
import {
  findEditableUserMessageIndex,
  isDurableChatMessageId,
} from "./chatAreaModel";
import {
  readAutoContinueAfterResumeWaitPreference,
  sessionViewCacheStore,
} from "./chatRuntimeModel";
import { useChatViewStore } from "./chatViewStore";
import { useChatScrollFollow } from "./useChatScrollFollow";
import { useAgentRuntimeConfig } from "./useAgentRuntimeConfig";
import { useAgentContextUsage } from "./useAgentContextUsage";
import { useAgentStreamConnection } from "./useAgentStreamConnection";
import { useAgentStreamController } from "./useAgentStreamController";
import { useManualContextCompaction } from "./useManualContextCompaction";
import { useQueuedPromptLifecycle } from "./useQueuedPromptLifecycle";
import { useSessionViewCachePersistence } from "./useSessionViewCachePersistence";
import { useQuestionCompletionLifecycle } from "./useQuestionCompletionLifecycle";
import { useDurableTurnMessageIds } from "./useDurableTurnMessageIds";
import { useSessionRequestOwnership } from "./useSessionRequestOwnership";
import { useSessionViewHydrationController } from "./useSessionViewHydrationController";
import { useChatPromptTransactions } from "./useChatPromptTransactions";
import {
  catchUpTranscriptPatches,
  DesktopTranscriptView,
  loadTranscriptPage,
} from "./transcriptPaging";
import {
  clearTranscriptContentRangeCache,
  subscribeTranscriptContentRangeCache,
  transcriptContentRangeCacheBytes,
} from "./transcriptContentRanges";
import {
  useAssistantTurnUpdateQueue,
  type AssistantTurnCommitOptions,
} from "./useAssistantTurnUpdateQueue";
import { VirtualMessageList } from "./VirtualMessageList";
import type {
  ChatAreaProps, ChatViewMode, ChatMessage,
  PendingQuestionState, CachedActiveReplay,
} from "./types";
const COMPOSER_BOTTOM_GAP_PX = 18;
const COMPOSER_SCROLL_GUTTER_PX = 18;
const TRANSCRIPT_MEMORY_WARNING_BYTES = 32 * 1024 * 1024;

const HYDRATION_STAGE_LABELS: Record<string, string> = {
  fetchProjection: t("chatArea.loadingConversationProjection"),
  reduceReplays: t("chatArea.mergingTaskReplay"),
  reduceMessages: t("chatArea.mergingMessageHistory"),
  finalizeSnapshot: t("chatArea.preparingRecoverySnapshot"),
  applySnapshot: t("chatArea.applyingConversationView"),
  refreshCachedSession: t("chatArea.validatingConversationCache"),
  fetchDeltaReplays: t("chatArea.loadingIncrementalReplay"),
  applyDeltaReplays: t("chatArea.applyingIncrementalReplay"),
};

type ChatViewMeta = {
  hasMessages: boolean;
  latestUserMessageId: string | null;
  editableUserMessageId: string | null;
};

const EMPTY_CHAT_VIEW_META: ChatViewMeta = {
  hasMessages: false,
  latestUserMessageId: null,
  editableUserMessageId: null,
};

const areChatViewMetaEqual = (
  left: ChatViewMeta,
  right: ChatViewMeta,
): boolean =>
  left.hasMessages === right.hasMessages &&
  left.latestUserMessageId === right.latestUserMessageId &&
  left.editableUserMessageId === right.editableUserMessageId;

const deriveChatViewMeta = (
  messages: ChatMessage[],
  options: {
    isStreaming: boolean;
    hasPendingQuestion: boolean;
  },
): ChatViewMeta => {
  let latestUserMessageId: string | null = null;
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const item = messages[index];
    if (item.role === "user") {
      latestUserMessageId = item.id;
      break;
    }
  }

  let editableUserMessageId: string | null = null;
  if (
    !options.isStreaming &&
    !options.hasPendingQuestion
  ) {
    const index = findEditableUserMessageIndex(messages);
    const item = index >= 0 ? messages[index] : null;
    const tail = messages.at(-1);
    if (
      item?.role === "user" &&
      isDurableChatMessageId("user", item.id) &&
      !(
        tail?.role === "assistant" &&
        !isDurableChatMessageId("assistant", tail.id)
      )
    ) {
      editableUserMessageId = item.id;
    }
  }

  return {
    hasMessages: messages.length > 0,
    latestUserMessageId,
    editableUserMessageId,
  };
};
export function ChatArea({
  currentSession,
  currentSessionId: selectedSessionId,
  workspaceName,
  workspaceRoot,
  gitStatus,
  gitStatusError,
  githubCliStatus,
  runtimeConfigRevision = 0,
  isPinnedSummaryOpen = false,
  isPinnedSummaryRetracting = false,
  onOpenWorkspacePath,
  onOpenAgentSession,
  onNewSession,
  onOpenResource,
  onSessionResolved,
  onAgentRunningChange,
  onSessionCompleted,
}: ChatAreaProps) {
  const [inputValue, setInputValue] = useState("");
  const composerContainerRef = useRef<HTMLDivElement | null>(null);
  const flushAssistantTurnUpdatesRef = useRef<() => void>(() => {});
  const streamConnection = useAgentStreamConnection(
    flushAssistantTurnUpdatesRef,
  );
  const {
    getActiveStream,
    isStreaming,
    setIsStreaming,
    closeActiveStream,
  } = streamConnection;
  const [autoContinueAfterResumeWait, setAutoContinueAfterResumeWait] =
    useState<boolean | undefined>(() =>
      readAutoContinueAfterResumeWaitPreference(),
    );
  const [runtimeConfigError, setRuntimeConfigError] = useState("");
  const [sessionLoadError, setSessionLoadError] = useState("");
  const [transcriptHasOlder, setTranscriptHasOlder] = useState(false);
  const [transcriptManagedBytes, setTranscriptManagedBytes] = useState(0);
  const [isLoadingOlderTranscript, setIsLoadingOlderTranscript] =
    useState(false);
  const [pendingQuestion, setPendingQuestion] =
    useState<PendingQuestionState | null>(null);
  const [pendingQuestionError, setPendingQuestionError] = useState("");
  const [chatViewMeta, setChatViewMeta] = useState<ChatViewMeta>(
    EMPTY_CHAT_VIEW_META,
  );
  const [editingUserMessageId, setEditingUserMessageId] = useState<
    string | null
  >(null);
  const [editingPrompt, setEditingPrompt] = useState("");
  const [copiedUserMessageId, setCopiedUserMessageId] = useState<string | null>(
    null,
  );
  const stoppedAssistantMessageIdsRef = useRef<Set<string>>(new Set());
  const pendingResolvedSessionRef = useRef<UiSession | null>(null);
  const preserveResolvedSessionIdRef = useRef<string | null>(null);
  const copiedUserMessageTimeoutRef = useRef<number | null>(null);
  const messagesRef = useRef<ChatMessage[]>([]);
  const messageIndexByIdRef = useRef<Map<string, number>>(new Map());
  const chatViewMetaRef = useRef<ChatViewMeta>(EMPTY_CHAT_VIEW_META);
  const pendingQuestionRef = useRef<PendingQuestionState | null>(null);
  const replayCursorsByAgentRunIdRef = useRef<SessionReplayCursors>({});
  const verifiedReplayAgentRunIdsRef = useRef<Set<string>>(new Set());
  const visibleSessionIdRef = useRef("");
  const visibleActiveReplayRef = useRef<CachedActiveReplay | null>(null);
  const transcriptViewRef = useRef<DesktopTranscriptView | null>(null);
  const transcriptHistoryMessageCountRef = useRef(0);
  const transcriptPatchRefreshInFlightRef = useRef<Promise<void> | null>(null);
  const transcriptPatchRefreshRequestedRef = useRef(false);
  const olderTranscriptRequestRef = useRef<Promise<void> | null>(null);
  const currentSessionId =
    selectedSessionId === undefined
      ? currentSession?.id || ""
      : selectedSessionId?.trim() || "";
  const {
    adoptSession,
    captureSessionRequest,
    ownsSessionRequest,
  } = useSessionRequestOwnership(currentSessionId);
  const {
    queuedPrompt: queuedNextPrompt,
    queuePrompt: setQueuedNextPromptText,
    takeQueuedPromptForSession,
    takeQueuedPromptForEditing,
    recoverQueuedPromptForStop,
  } = useQueuedPromptLifecycle(currentSessionId);
  const {
    contextUsage,
    applyContextUsage,
    resetContextUsage,
    markContextCompacting,
    refreshContextUsage,
  } = useAgentContextUsage(currentSessionId);
  const {
    isManualCompacting,
    compactContext: handleCompactContext,
  } = useManualContextCompaction({
    sessionId: currentSession?.id || "",
    isStreaming,
    refreshContextUsage,
    onError: setRuntimeConfigError,
  });
  const {
    messagesContainerRef,
    contentRef: messagesContentRef,
    spacerRef: messagesSpacerRef,
    pauseFollowing,
    isFollowingLatest,
    handleMessagesScroll,
    scheduleFollowLatestScroll,
    handleJumpToLatest,
    resumeFollowingLatest,
  } = useChatScrollFollow(currentSessionId);
  const {
    persistVisibleSessionViewCache,
    scheduleVisibleSessionViewCachePersist,
  } = useSessionViewCachePersistence({
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
  });
  const [composerHeight, setComposerHeight] = useState(0);
  const commitMessagesToView = useCallback(
    (
      nextMessages: ChatMessage[],
      options: AssistantTurnCommitOptions = {},
    ) => {
      messagesRef.current = nextMessages;
      if (options.assistantMessages) {
        useChatViewStore
          .getState()
          .updateAssistantMessages(options.assistantMessages);
      } else {
        const indexById = new Map<string, number>();
        nextMessages.forEach((message, index) => {
          indexById.set(message.id, index);
        });
        messageIndexByIdRef.current = indexById;
        useChatViewStore.getState().replaceMessages(nextMessages);
      }

      if (options.refreshMeta !== false) {
        const nextMeta = deriveChatViewMeta(nextMessages, {
          isStreaming: Boolean(getActiveStream()),
          hasPendingQuestion: Boolean(pendingQuestionRef.current),
        });
        if (!areChatViewMetaEqual(chatViewMetaRef.current, nextMeta)) {
          chatViewMetaRef.current = nextMeta;
          setChatViewMeta(nextMeta);
        }
      }
      scheduleVisibleSessionViewCachePersist();
    },
    [getActiveStream, scheduleVisibleSessionViewCachePersist],
  );

  const setMessages = useCallback(
    (
      updater:
        | ChatMessage[]
        | ((previous: ChatMessage[]) => ChatMessage[]),
    ) => {
      const previous = messagesRef.current;
      const next =
        typeof updater === "function" ? updater(previous) : updater;
      if (next === previous) {
        return;
      }
      commitMessagesToView(next);
    },
    [commitMessagesToView],
  );

  const syncTranscriptView = useCallback(() => {
    const view = transcriptViewRef.current;
    if (!view || visibleSessionIdRef.current !== view.sessionId) {
      return;
    }
    const liveOverlayActive = Boolean(getActiveStream());
    const historyMessages = view.materializeMessages(liveOverlayActive);
    const suffix = messagesRef.current
      .slice(transcriptHistoryMessageCountRef.current)
      .filter(
        (message) =>
          liveOverlayActive ||
          message.id.startsWith("assistant-restore-message-"),
      );
    transcriptHistoryMessageCountRef.current = historyMessages.length;
    setTranscriptHasOlder(view.hasOlder);
    setTranscriptManagedBytes(
      view.managedContentBytes + transcriptContentRangeCacheBytes(),
    );
    setMessages([...historyMessages, ...suffix]);
  }, [getActiveStream, setMessages]);

  useEffect(
    () =>
      subscribeTranscriptContentRangeCache(() => {
        const view = transcriptViewRef.current;
        setTranscriptManagedBytes(
          (view?.managedContentBytes ?? 0) + transcriptContentRangeCacheBytes(),
        );
      }),
    [],
  );

  const scheduleTranscriptPatchRefresh = useCallback(() => {
    transcriptPatchRefreshRequestedRef.current = true;
    if (transcriptPatchRefreshInFlightRef.current) {
      return;
    }
    const task = (async () => {
      while (transcriptPatchRefreshRequestedRef.current) {
        transcriptPatchRefreshRequestedRef.current = false;
        const view = transcriptViewRef.current;
        const expectedSessionId = visibleSessionIdRef.current;
        if (!view || view.sessionId !== expectedSessionId) {
          continue;
        }
        await catchUpTranscriptPatches(view);
        if (
          transcriptViewRef.current !== view ||
          visibleSessionIdRef.current !== expectedSessionId
        ) {
          continue;
        }
        flushAssistantTurnUpdatesRef.current();
        syncTranscriptView();
      }
    })()
      .catch((error: unknown) => {
        const detail =
          error instanceof Error && error.message.trim()
            ? error.message.trim()
            : "unknown transcript update error";
        setRuntimeConfigError(`Transcript update failed: ${detail}`);
      })
      .finally(() => {
        transcriptPatchRefreshInFlightRef.current = null;
      });
    transcriptPatchRefreshInFlightRef.current = task;
  }, [syncTranscriptView]);

  const { applyDurableTurnMessageIds } = useDurableTurnMessageIds({
    setMessages,
    getActiveStream,
    stoppedAssistantMessageIdsRef,
  });

  useEffect(() => {
    pendingQuestionRef.current = pendingQuestion;
    const nextMeta = deriveChatViewMeta(messagesRef.current, {
      isStreaming: Boolean(getActiveStream()),
      hasPendingQuestion: Boolean(pendingQuestion),
    });
    if (!areChatViewMetaEqual(chatViewMetaRef.current, nextMeta)) {
      chatViewMetaRef.current = nextMeta;
      setChatViewMeta(nextMeta);
    }
  }, [getActiveStream, pendingQuestion]);

  useEffect(() => {
    const nextMeta = deriveChatViewMeta(messagesRef.current, {
      isStreaming,
      hasPendingQuestion: Boolean(pendingQuestionRef.current),
    });
    if (!areChatViewMetaEqual(chatViewMetaRef.current, nextMeta)) {
      chatViewMetaRef.current = nextMeta;
      setChatViewMeta(nextMeta);
    }
  }, [isStreaming]);

  useEffect(() => {
    const element = composerContainerRef.current;
    if (!element) {
      return undefined;
    }

    const updateComposerHeight = () => {
      setComposerHeight(Math.ceil(element.getBoundingClientRect().height));
    };
    updateComposerHeight();

    const resizeObserver = new ResizeObserver(updateComposerHeight);
    resizeObserver.observe(element);
    return () => resizeObserver.disconnect();
  }, []);

  const hasInput = useMemo(() => inputValue.trim().length > 0, [inputValue]);
  const chatContainerStyle = useMemo(
    () =>
      ({
        "--uiRsComposerScrollClearance": `${
          Math.max(
            172,
            composerHeight + COMPOSER_BOTTOM_GAP_PX + COMPOSER_SCROLL_GUTTER_PX,
          )
        }px`,
      }) as CSSProperties,
    [composerHeight],
  );
  const hasVisibleConversation =
    chatViewMeta.hasMessages || Boolean(pendingQuestion);
  const hasSelectedSession = currentSessionId.length > 0;
  const chatViewMode: ChatViewMode = hasVisibleConversation
    ? "conversation"
    : hasSelectedSession
      ? "restoring"
      : "welcome";
  const editableUserMessageId = chatViewMeta.editableUserMessageId;
  const latestUserMessageId = chatViewMeta.latestUserMessageId;
  const activeWorkspaceRoot = workspaceRoot ?? undefined;
  const shouldShowWorkspaceLabel = Boolean(activeWorkspaceRoot);
  const shouldShowPinnedSummary = Boolean(
    currentSession || hasVisibleConversation,
  );
  const {
    selectableModels,
    activeModelIndex,
    reasoningEfforts,
    reasoningEffort,
    modelRuntimeSummary,
    applyGlobalRuntimeConfig,
    selectGlobalModel,
    selectReasoningEffort,
  } = useAgentRuntimeConfig({
    revision: runtimeConfigRevision,
    onError: setRuntimeConfigError,
  });
  const rememberReplayPayloads = useCallback(
    (payloads: readonly AgentStreamPayload[]) => {
      if (payloads.some((payload) => payload.type === "session_event")) {
        queueMicrotask(scheduleTranscriptPatchRefresh);
      }
      const patch = deriveReplayCursorPatch(payloads);
      if (Object.keys(patch).length === 0) {
        return;
      }
      replayCursorsByAgentRunIdRef.current = mergeReplayCursors(
        replayCursorsByAgentRunIdRef.current,
        patch,
      );
      Object.keys(patch).forEach((agentRunId) => {
        verifiedReplayAgentRunIdsRef.current.add(agentRunId);
      });
      const visibleSessionId = visibleSessionIdRef.current;
      if (visibleSessionId) {
        sessionViewCacheStore.patchReplayCursors(visibleSessionId, patch);
      }
    },
    [scheduleTranscriptPatchRefresh],
  );
  const assistantTurnUpdates = useAssistantTurnUpdateQueue({
    messagesRef,
    messageIndexByIdRef,
    commitMessagesToView,
    flushAssistantTurnUpdatesRef,
  });
  const {
    updateAssistantTurn,
    flushAssistantTurnUpdates,
  } = assistantTurnUpdates;
  const {
    processStreamPayload,
    startStreamForAssistant,
    clearStreamEventHistory,
  } = useAgentStreamController({
    connection: streamConnection,
    turnUpdates: assistantTurnUpdates,
    context: { markContextCompacting, refreshContextUsage },
    question: { setPendingQuestion, setPendingQuestionError },
    sessionOutcome: {
      pendingResolvedSessionRef,
      preserveResolvedSessionIdRef,
      onSessionResolved,
      onAgentRunningChange,
      onSessionCompleted,
    },
    replay: { rememberReplayPayloads, visibleActiveReplayRef },
  });

  const {
    handleQuestionOptionToggle,
    handleQuestionTextChange,
    handleQuestionSubmit,
  } = useQuestionCompletionLifecycle({
    sessionId: currentSessionId,
    autoContinueAfterResumeWait,
    pendingQuestion,
    setPendingQuestion,
    setPendingQuestionError,
    setMessages,
    startStreamForAssistant,
    refreshContextUsage,
    updateAssistantTurn,
    onAgentRunningChange,
  });

  const { isHydratingSession, hydrationStage } =
    useSessionViewHydrationController({
      view: {
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
      },
      replay: {
        replayCursorsByAgentRunIdRef,
        verifiedReplayAgentRunIdsRef,
        visibleActiveReplayRef,
        persistVisibleSessionViewCache,
      },
      runtime: {
        setAutoContinueAfterResumeWait,
        applyGlobalRuntimeConfig,
        applyContextUsage,
        resetContextUsage,
      },
      question: { setPendingQuestion, setPendingQuestionError },
      stream: {
        getActiveStream,
        closeActiveStream,
        setIsStreaming,
        processStreamPayload,
        startStreamForAssistant,
        clearStreamEventHistory,
        updateAssistantTurn,
        flushAssistantTurnUpdates,
      },
      sessionOutcome: {
        pendingResolvedSessionRef,
        preserveResolvedSessionIdRef,
      },
    });

  useEffect(() => {
    if (
      !isHydratingSession &&
      transcriptViewRef.current?.sessionId === currentSessionId
    ) {
      scheduleTranscriptPatchRefresh();
    }
  }, [
    currentSessionId,
    isHydratingSession,
    scheduleTranscriptPatchRefresh,
  ]);

  const handleLoadOlderTranscript = useCallback(() => {
    const view = transcriptViewRef.current;
    const olderCursor = view?.olderCursor;
    if (!view || !olderCursor || olderTranscriptRequestRef.current) {
      return;
    }
    const expectedSessionId = view.sessionId;
    const expectedReleaseRevision = view.currentReleaseRevision;
    const scrollElement = messagesContainerRef.current;
    const previousScrollHeight = scrollElement?.scrollHeight ?? 0;
    const previousScrollTop = scrollElement?.scrollTop ?? 0;
    setIsLoadingOlderTranscript(true);
    const task = loadTranscriptPage({
      sessionId: view.sessionId,
      projectionGeneration: view.projectionGeneration,
      sourceHighWater: view.sourceHighWater,
      olderCursor,
    })
      .then((page) => {
        if (
          transcriptViewRef.current !== view ||
          visibleSessionIdRef.current !== expectedSessionId ||
          view.currentReleaseRevision !== expectedReleaseRevision
        ) {
          return;
        }
        view.applyOlderPage(page);
        syncTranscriptView();
        requestAnimationFrame(() => {
          if (
            scrollElement &&
            transcriptViewRef.current === view &&
            visibleSessionIdRef.current === expectedSessionId
          ) {
            scrollElement.scrollTop =
              previousScrollTop +
              Math.max(0, scrollElement.scrollHeight - previousScrollHeight);
          }
        });
      })
      .catch((error: unknown) => {
        const detail =
          error instanceof Error && error.message.trim()
            ? error.message.trim()
            : "unknown transcript paging error";
        setRuntimeConfigError(`Loading earlier messages failed: ${detail}`);
      })
      .finally(() => {
        if (olderTranscriptRequestRef.current === task) {
          olderTranscriptRequestRef.current = null;
          setIsLoadingOlderTranscript(false);
        }
      });
    olderTranscriptRequestRef.current = task;
  }, [messagesContainerRef, syncTranscriptView]);

  const handleReleaseTranscriptHistory = useCallback(() => {
    const view = transcriptViewRef.current;
    if (!view) return;
    clearTranscriptContentRangeCache();
    view.releaseLoadedHistory();
    syncTranscriptView();
    requestAnimationFrame(() => handleJumpToLatest());
  }, [handleJumpToLatest, syncTranscriptView]);

  const {
    sendPrompt,
    sendPromptAsSupplement,
    stopActiveAgentRun,
    submitEditedUserMessage,
  } = useChatPromptTransactions({
    session: {
      currentSession,
      currentSessionId,
      workspaceRoot: activeWorkspaceRoot,
      autoContinueAfterResumeWait,
      ownership: { adoptSession, captureSessionRequest, ownsSessionRequest },
      pendingResolvedSessionRef,
      preserveResolvedSessionIdRef,
      onSessionResolved,
      onAgentRunningChange,
    },
    view: {
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
    },
    stream: {
      connection: streamConnection,
      turnUpdates: assistantTurnUpdates,
      applyDurableTurnMessageIds,
      startStreamForAssistant,
      refreshContextUsage,
    },
    queue: {
      queuePrompt: setQueuedNextPromptText,
      takeQueuedPromptForSession,
      recoverQueuedPromptForStop,
    },
    navigation: {
      jumpToLatest: handleJumpToLatest,
      resumeFollowingLatest,
    },
  });

  useEffect(() => {
    return () => {
      closeActiveStream();
      if (copiedUserMessageTimeoutRef.current !== null) {
        window.clearTimeout(copiedUserMessageTimeoutRef.current);
        copiedUserMessageTimeoutRef.current = null;
      }
    };
  }, [closeActiveStream]);

  const handleComposerAction = useCallback(() => {
    if (hasInput) {
      void sendPrompt();
      return;
    }
    if (isStreaming) {
      stopActiveAgentRun();
    }
  }, [hasInput, isStreaming, sendPrompt, stopActiveAgentRun]);

  const handleEditQueuedNextPrompt = useCallback(() => {
    const prompt = takeQueuedPromptForEditing(currentSessionId);
    setInputValue(prompt);
  }, [currentSessionId, takeQueuedPromptForEditing]);

  const handleSubmitQueuedNextPromptAsSupplement = useCallback(() => {
    const activeStream = getActiveStream();
    const prompt = takeQueuedPromptForSession(
      activeStream?.sessionId ?? currentSessionId,
    );
    if (!prompt) {
      return;
    }
    if (activeStream) {
      void sendPromptAsSupplement(prompt, activeStream);
      return;
    }
    void sendPrompt(prompt);
  }, [
    currentSessionId,
    getActiveStream,
    sendPrompt,
    sendPromptAsSupplement,
    takeQueuedPromptForSession,
  ]);

  const handleStartEditingUserMessage = useCallback(
    (message: Extract<ChatMessage, { role: "user" }>) => {
      if (isStreaming) {
        return;
      }
      const editableIndex = findEditableUserMessageIndex(messagesRef.current);
      if (
        editableIndex < 0 ||
        messagesRef.current[editableIndex]?.id !== message.id
      ) {
        return;
      }
      setEditingUserMessageId(message.id);
      setEditingPrompt(message.text);
    },
    [isStreaming],
  );

  const handleCancelEditingUserMessage = useCallback(() => {
    setEditingUserMessageId(null);
    setEditingPrompt("");
  }, []);

  const handleCopyUserMessage = useCallback(
    async (messageId: string, text: string) => {
      const normalized = text.trim();
      if (
        !normalized ||
        typeof navigator === "undefined" ||
        !navigator.clipboard
      ) {
        return;
      }
      try {
        await navigator.clipboard.writeText(normalized);
        setCopiedUserMessageId(messageId);
        if (copiedUserMessageTimeoutRef.current !== null) {
          window.clearTimeout(copiedUserMessageTimeoutRef.current);
        }
        copiedUserMessageTimeoutRef.current = window.setTimeout(() => {
          setCopiedUserMessageId((current) =>
            current === messageId ? null : current,
          );
          copiedUserMessageTimeoutRef.current = null;
        }, 2000);
      } catch {
        // Clipboard failures should not disturb the conversation.
      }
    },
    [],
  );

  const handleSubmitEditedUserMessageAction = useCallback(
    (messageId: string) => {
      void submitEditedUserMessage(messageId);
    },
    [submitEditedUserMessage],
  );
  const handleCopyUserMessageAction = useCallback(
    (messageId: string, text: string) => {
      void handleCopyUserMessage(messageId, text);
    },
    [handleCopyUserMessage],
  );
  const handleQuestionSubmitAction = useCallback(() => {
    void handleQuestionSubmit();
  }, [handleQuestionSubmit]);
  const handleSendAction = useCallback(() => {
    void sendPrompt();
  }, [sendPrompt]);
  const handleModelSelect = useCallback(
    (configured: Parameters<typeof selectGlobalModel>[0]) => {
      void selectGlobalModel(configured);
    },
    [selectGlobalModel],
  );
  const handleReasoningEffortSelect = useCallback(
    (effort: Parameters<typeof selectReasoningEffort>[0]) => {
      void selectReasoningEffort(effort);
    },
    [selectReasoningEffort],
  );
  const handleCompactAction = useCallback(() => {
    void handleCompactContext();
  }, [handleCompactContext]);

  return (
    <div
      className={`chat-container ${chatViewMode !== "welcome" ? "has-conversation" : "is-empty"}`}
      style={chatContainerStyle}
    >
      {isPinnedSummaryOpen && shouldShowPinnedSummary ? (
        <section
          className={`pinnedSummaryCard ${isPinnedSummaryRetracting ? "is-retracting" : ""}`}
          aria-label={t("chatArea.summaryStatus")}
        >
          <header className="pinnedSummaryHeader">
            <span>{t("chatArea.environment")}</span>
          </header>
          <div className="pinnedSummaryRows">
            <div className="pinnedSummaryRow">
              <HardDrive className="pinnedSummaryIcon" aria-hidden="true" />
              <span>{t("chatArea.local")}</span>
              <strong>{shouldShowWorkspaceLabel ? workspaceName : t("chatArea.noWorkspaceSelected")}</strong>
            </div>
            {gitStatus ? (
              <>
                <div className="pinnedSummaryRow">
                  <FileText className="pinnedSummaryIcon" aria-hidden="true" />
                  <span>{t("chatArea.changes")}</span>
                  <strong className="pinnedSummaryDiff">
                    <span className="is-added">+{gitStatus.totalAdded.toLocaleString()}</span>
                    <span className="is-removed">-{gitStatus.totalRemoved.toLocaleString()}</span>
                  </strong>
                </div>
                <div className="pinnedSummaryRow">
                  <GitBranch className="pinnedSummaryIcon" aria-hidden="true" />
                  <span>{t("chatArea.branch")}</span>
                  <strong>{gitStatus.branch || "detached"}</strong>
                </div>
              </>
            ) : (
              <div className="pinnedSummaryRow">
                <GitBranch className="pinnedSummaryIcon" aria-hidden="true" />
                <span>Git</span>
                <strong>{gitStatusError || t("chatArea.gitStatusDisconnected")}</strong>
              </div>
            )}
            <div className="pinnedSummaryRow">
              <CloudUpload className="pinnedSummaryIcon" aria-hidden="true" />
              <span>{t("chatArea.commitOrPush")}</span>
              <strong>{t("chatArea.disconnected")}</strong>
            </div>
            <div className="pinnedSummaryRow">
              <GitHubMarkIcon className="pinnedSummaryIcon" aria-hidden="true" />
              <span>GitHub CLI</span>
              <strong>{githubCliStatus?.summary ?? t("chatArea.githubCliNotDetected")}</strong>
            </div>
          </div>
          <div className="pinnedSummaryDivider" />
          <div className="pinnedSummarySource">
            <span>{t("chatArea.sources")}</span>
            <p>{t("chatArea.noSourcesYet")}</p>
          </div>
        </section>
      ) : null}

      <div
        className="chat-main"
      >
        <div className="chatContentPane">
          {sessionLoadError ? (
            <section
              className="session-load-error"
              data-chat-view-mode="error"
              role="alert"
            >
              <h2>{t("chatArea.unableToLoadConversation")}</h2>
              <p>{sessionLoadError}</p>
            </section>
          ) : chatViewMode === "welcome" ? null : chatViewMode === "restoring" ? (
            <div
              className="conversation-restore-section"
              data-chat-view-mode="restoring"
            >
              <span>
                {isHydratingSession
                  ? t("chatArea.restoringConversationValue", { value1: hydrationStage ? `：${HYDRATION_STAGE_LABELS[hydrationStage] || hydrationStage}` : "" })
                  : t("chatArea.noMessagesInThisConversationYet")}
              </span>
            </div>
          ) : (
            <>
            {transcriptManagedBytes >= TRANSCRIPT_MEMORY_WARNING_BYTES ? (
              <div className="transcript-memory-warning" role="status">
                <span>{t("chatArea.loadedConversationDataExceeds32MiB")}</span>
                <button type="button" onClick={handleReleaseTranscriptHistory}>
                  {t("chatArea.releaseHistoryAndReturnToLatest")}
                </button>
              </div>
            ) : null}
            <VirtualMessageList
              containerRef={messagesContainerRef}
              contentRef={messagesContentRef}
              spacerRef={messagesSpacerRef}
              onReadingIntent={pauseFollowing}
              hasOlder={transcriptHasOlder}
              isLoadingOlder={isLoadingOlderTranscript}
              onLoadOlder={handleLoadOlderTranscript}
              editingUserMessageId={editingUserMessageId}
              editingPrompt={editingPrompt}
              copiedUserMessageId={copiedUserMessageId}
              latestUserMessageId={latestUserMessageId}
              editableUserMessageId={editableUserMessageId}
              onOpenAgentSession={onOpenAgentSession}
              onScroll={handleMessagesScroll}
              onContentSizeChange={scheduleFollowLatestScroll}
              onEditingPromptChange={setEditingPrompt}
              onCancelEditingUserMessage={handleCancelEditingUserMessage}
              onSubmitEditedUserMessage={handleSubmitEditedUserMessageAction}
              onCopyUserMessage={handleCopyUserMessageAction}
              onStartEditingUserMessage={handleStartEditingUserMessage}
              onOpenWorkspacePath={onOpenWorkspacePath}
            />
            </>
          )}
        </div>
        {!sessionLoadError && !isFollowingLatest && chatViewMode === "conversation" ? (
          <button
            type="button"
            className="jump-to-latest"
            onClick={handleJumpToLatest}
            aria-label={t("chatArea.jumpToLatest")}
            title={t("chatArea.jumpToLatest")}
          >
            <ChevronDown aria-hidden="true" />
          </button>
        ) : null}
      </div>

      {!sessionLoadError ? (
        <div
          className={`chatBottomPlane ${chatViewMode === "welcome" ? "is-welcome" : ""}`}
        >
        {chatViewMode === "welcome" ? (
          <div className="emptyChatIdentity">
            <strong>Centaeris</strong>
            <span>Run /help for commands</span>
          </div>
        ) : null}
        <div className="input-section">
          <div className="input-container" ref={composerContainerRef}>
            {pendingQuestion ? (
              <PendingQuestionPanel
                pendingQuestion={pendingQuestion}
                pendingQuestionError={pendingQuestionError}
                onOptionToggle={handleQuestionOptionToggle}
                onTextChange={handleQuestionTextChange}
                onSubmit={handleQuestionSubmitAction}
              />
            ) : null}

            {queuedNextPrompt.trim() ? (
              <div className="queuedNextPromptDrawer">
                <div className="queuedNextPromptText">
                  {queuedNextPrompt.trim()}
                </div>
                <div className="queuedNextPromptActions">
                  <Tooltip content={t("chatArea.edit")}>
                    <Button
                      type="button"
                      variant="composerIcon"
                      size="composerIcon"
                      className="queuedNextPromptButton"
                      aria-label={t("chatArea.editQueuedInput")}
                      onClick={handleEditQueuedNextPrompt}
                    >
                      <Pencil
                        className="composerLucideIcon"
                        aria-hidden="true"
                      />
                    </Button>
                  </Tooltip>
                  <Tooltip align="end" content={t("chatArea.sendNow")}>
                    <Button
                      type="button"
                      variant="composerIcon"
                      size="composerIcon"
                      className="queuedNextPromptButton"
                      aria-label={t("chatArea.sendQueuedInputNow")}
                      onClick={handleSubmitQueuedNextPromptAsSupplement}
                    >
                      <CornerDownLeft
                        className="composerLucideIcon"
                        aria-hidden="true"
                      />
                    </Button>
                  </Tooltip>
                </div>
              </div>
            ) : null}

            <ChatComposer
              panelResetKey={currentSession?.id ?? "welcome"}
              inputValue={inputValue}
              isStreaming={isStreaming}
              hasInput={hasInput}
              modelRuntimeSummary={modelRuntimeSummary}
              selectableModels={selectableModels}
              activeModelIndex={activeModelIndex}
              runtimeConfigError={runtimeConfigError}
              reasoningEffort={reasoningEffort}
              reasoningEfforts={reasoningEfforts}
              contextUsage={contextUsage}
              compactInteractive={chatViewMode === "conversation"}
              isCompacting={chatViewMode === "conversation" && Boolean(
                contextUsage?.isCompacting || isManualCompacting,
              )}
              onInputChange={setInputValue}
              onSubmit={handleSendAction}
              onComposerAction={handleComposerAction}
              onModelSelect={handleModelSelect}
              onReasoningEffortSelect={handleReasoningEffortSelect}
              onCompact={handleCompactAction}
              onNewSession={onNewSession}
              onOpenResource={onOpenResource}
            />
          </div>
        </div>
        </div>
      ) : null}

    </div>
  );
}
