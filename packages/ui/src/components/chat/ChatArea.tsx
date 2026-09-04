import {
  type CSSProperties,
  useCallback,
  useEffect,
  useLayoutEffect,
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
  answerAgentQuestion,
  cancelAgentRun,
  compactAgentContext,
  createSession,
  getAgentRuntimeConfig,
  getAgentContextUsage,
  getSession,
  listAgentRuns,
  openAgentStream,
  sendAgentSupplement,
  sendAgentInput,
  setAgentRuntimeConfig,
  type AgentContextUsageSummary,
  type AgentStreamPayload,
  type AgentRunSummary,
  type ModelThinkingMode,
  type SelectableModel,
  type SessionEvent,
} from "../../lib/chatBridge";
import {
  decideSessionViewCacheReplay,
  deriveReplayCursorPatch,
  mergeReplayCursors,
  type SessionReplayCursors,
  type SessionViewCacheEntry,
} from "../../lib/sessionViewCache";
import { Button } from "../ui/button";
import { Tooltip } from "../ui/tooltip";
import { isNativeHostRuntime } from "../../host/hostBridge";
import { ChatComposer } from "./ChatComposer";
import { PendingQuestionPanel } from "./ChatPendingPanels";
import {
  appendNarrativeChunk,
  findAssistantMessageIdForAgentRunInView,
  findEditableUserMessageIndex,
  flushDraftAnswerToNarrative,
  isDurableChatMessageId,
  isNearScrollBottom,
  prunePendingTailMessages,
  recoverQueuedPromptAfterStop,
  setTurnActivity,
  waitForNextPaint,
} from "./chatAreaModel";
import { runtimeEasterEgg } from "./chatRuntimeCore";
import {
  AUTO_CONTINUE_AFTER_RESUME_WAIT_KEY,
  EMPTY_MODEL_RUNTIME_DRAFT,
  RUNTIME_ACTIVITY_BY_PROCESS_STATE,
  applySessionEventToAssistantTurn,
  appendGuidedSupplementChunk,
  buildPendingTurn,
  buildModelRuntimeDraft,
  buildSeenSetsFromStreamPayloads,
  buildSessionHydrationSnapshot,
  collectSessionVisibleMessageIds,
  formatExecutionError,
  formatRuntimeModelError,
  getEventTurnId,
  getSessionEventId,
  getTerminalSessionEventStatus,
  isActiveAgentRun,
  isRecord,
  makeId,
  mapPreparingToolNameToActivity,
  mapProcessStateToActivity,
  mapRuntimePayloadToActivity,
  normalizeAgentRunId,
  normalizeRuntimeActivity,
  parsePendingQuestionRequest,
  readAutoContinueAfterResumeWaitPreference,
  readHydrationValue,
  replayAgentRunStreamFromCursor,
  selectReplayAgentRun,
  sessionViewCacheStore,
} from "./chatRuntimeModel";
import { useChatViewStore } from "./chatViewStore";
import { VirtualMessageList } from "./VirtualMessageList";
import type {
  ChatAreaProps, ChatViewMode, ChatMessage,
  RuntimeActivity,
  AssistantExecutionTurn, ActiveStreamState, StreamSeenSets,
  PendingQuestionState, ModelRuntimeDraft, CachedActiveReplay,
  SessionViewSnapshot, SessionHydrationSnapshot,
} from "./types";
const SESSION_VIEW_CACHE_WRITE_DELAY_MS = 500;
const HYDRATION_DELTA_PAYLOAD_BATCH_SIZE = 24;
const COMPOSER_BOTTOM_GAP_PX = 18;
const COMPOSER_SCROLL_GUTTER_PX = 18;

const HYDRATION_STAGE_LABELS: Record<string, string> = {
  fetchProjection: "读取会话投影",
  reduceReplays: "归并任务回放",
  reduceMessages: "归并历史消息",
  finalizeSnapshot: "整理恢复快照",
  applySnapshot: "应用会话视图",
  refreshCachedSession: "校验会话缓存",
  fetchDeltaReplays: "读取增量回放",
  applyDeltaReplays: "应用增量回放",
};

type AssistantTurnUpdater = (
  turn: AssistantExecutionTurn,
) => AssistantExecutionTurn;

type PendingAssistantTurnUpdate =
  | { kind: "textDelta"; delta: string }
  | { kind: "update"; updater: AssistantTurnUpdater };

type AssistantChatMessage = Extract<ChatMessage, { role: "assistant" }>;

type CommitMessagesOptions = {
  assistantMessages?: AssistantChatMessage[];
  refreshMeta?: boolean;
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
  const [isStreaming, setIsStreaming] = useState(false);
  const [autoContinueAfterResumeWait, setAutoContinueAfterResumeWait] =
    useState<boolean | undefined>(() =>
      readAutoContinueAfterResumeWaitPreference(),
    );
  const [modelRuntimeDraft, setModelRuntimeDraft] = useState<ModelRuntimeDraft>(
    () => ({
      ...EMPTY_MODEL_RUNTIME_DRAFT,
    }),
  );
  const [selectableModels, setSelectableModels] = useState<SelectableModel[]>([]);
  const [contextUsage, setContextUsage] =
    useState<AgentContextUsageSummary | null>(null);
  const [runtimeConfigError, setRuntimeConfigError] = useState("");
  const [sessionLoadError, setSessionLoadError] = useState("");
  const [pendingQuestion, setPendingQuestion] =
    useState<PendingQuestionState | null>(null);
  const [pendingQuestionError, setPendingQuestionError] = useState("");
  const [queuedNextPrompt, setQueuedNextPrompt] = useState("");
  const [chatViewMeta, setChatViewMeta] = useState<ChatViewMeta>(
    EMPTY_CHAT_VIEW_META,
  );
  const [isHydratingSession, setIsHydratingSession] = useState(false);
  const [hydrationStage, setHydrationStage] = useState("");
  const [editingUserMessageId, setEditingUserMessageId] = useState<
    string | null
  >(null);
  const [editingPrompt, setEditingPrompt] = useState("");
  const [copiedUserMessageId, setCopiedUserMessageId] = useState<string | null>(
    null,
  );
  const [isManualCompacting, setIsManualCompacting] = useState(false);
  const messagesContainerRef = useRef<HTMLDivElement | null>(null);
  const isFollowingLatestRef = useRef(true);
  const [isFollowingLatest, setIsFollowingLatest] = useState(true);
  const followLatestFrameRef = useRef<number | null>(null);
  const activeStreamRef = useRef<ActiveStreamState | null>(null);
  const streamSeenByMessageIdRef = useRef<Map<string, StreamSeenSets>>(
    new Map(),
  );
  const stoppedAssistantMessageIdsRef = useRef<Set<string>>(new Set());
  const queuedNextPromptRef = useRef("");
  const queuedNextPromptSessionIdRef = useRef("");
  const pendingResolvedSessionRef = useRef<UiSession | null>(null);
  const preserveResolvedSessionIdRef = useRef<string | null>(null);
  const hydrateRequestIdRef = useRef(0);
  const copiedUserMessageTimeoutRef = useRef<number | null>(null);
  const sessionViewCachePersistTimerRef = useRef<number | null>(null);
  const setQueuedNextPromptText = useCallback((value: string, sessionId = "") => {
    queuedNextPromptRef.current = value;
    queuedNextPromptSessionIdRef.current = value.trim() ? sessionId : "";
    setQueuedNextPrompt(value);
  }, []);
  const messagesRef = useRef<ChatMessage[]>([]);
  const messageIndexByIdRef = useRef<Map<string, number>>(new Map());
  const chatViewMetaRef = useRef<ChatViewMeta>(EMPTY_CHAT_VIEW_META);
  const pendingQuestionRef = useRef<PendingQuestionState | null>(null);
  const pendingQuestionErrorRef = useRef("");
  const contextUsageRef = useRef<AgentContextUsageSummary | null>(null);
  const autoContinueAfterResumeWaitRef = useRef<boolean | undefined>(
    autoContinueAfterResumeWait,
  );
  const replayCursorsByAgentRunIdRef = useRef<SessionReplayCursors>({});
  const verifiedReplayAgentRunIdsRef = useRef<Set<string>>(new Set());
  const visibleSessionIdRef = useRef("");
  const visibleActiveReplayRef = useRef<CachedActiveReplay | null>(null);
  const pendingAssistantTurnUpdatesRef = useRef<
    Map<string, PendingAssistantTurnUpdate[]>
  >(new Map());
  const assistantTurnUpdateFrameRef = useRef<number | null>(null);
  const processStreamPayloadRef = useRef<
    ((assistantMessageId: string, payload: AgentStreamPayload) => void) | null
  >(null);
  const scheduleVisibleSessionViewCachePersistRef = useRef<(() => void) | null>(
    null,
  );
  const currentSessionId =
    selectedSessionId === undefined
      ? currentSession?.id || ""
      : selectedSessionId?.trim() || "";
  const [composerHeight, setComposerHeight] = useState(0);

  useEffect(() => {
    if (
      queuedNextPromptRef.current.trim() &&
      queuedNextPromptSessionIdRef.current &&
      queuedNextPromptSessionIdRef.current !== currentSessionId
    ) {
      setQueuedNextPromptText("");
    }
  }, [currentSessionId, setQueuedNextPromptText]);
  const setFollowingLatest = useCallback((nextValue: boolean) => {
    if (isFollowingLatestRef.current === nextValue) {
      return;
    }
    isFollowingLatestRef.current = nextValue;
    setIsFollowingLatest(nextValue);
  }, []);
  const scheduleFollowLatestScroll = useCallback(() => {
    if (
      !isFollowingLatestRef.current ||
      followLatestFrameRef.current !== null
    ) {
      return;
    }
    followLatestFrameRef.current = window.requestAnimationFrame(() => {
      followLatestFrameRef.current = null;
      const container = messagesContainerRef.current;
      if (!container || !isFollowingLatestRef.current) {
        return;
      }
      container.scrollTop = container.scrollHeight;
    });
  }, []);
  useLayoutEffect(() => {
    setFollowingLatest(true);
    scheduleFollowLatestScroll();
  }, [currentSessionId, scheduleFollowLatestScroll, setFollowingLatest]);

  const commitMessagesToView = useCallback(
    (nextMessages: ChatMessage[], options: CommitMessagesOptions = {}) => {
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
          isStreaming: Boolean(activeStreamRef.current),
          hasPendingQuestion: Boolean(pendingQuestionRef.current),
        });
        if (!areChatViewMetaEqual(chatViewMetaRef.current, nextMeta)) {
          chatViewMetaRef.current = nextMeta;
          setChatViewMeta(nextMeta);
        }
      }
      scheduleVisibleSessionViewCachePersistRef.current?.();
    },
    [],
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

  useEffect(() => {
    pendingQuestionRef.current = pendingQuestion;
    const nextMeta = deriveChatViewMeta(messagesRef.current, {
      isStreaming: Boolean(activeStreamRef.current),
      hasPendingQuestion: Boolean(pendingQuestion),
    });
    if (!areChatViewMetaEqual(chatViewMetaRef.current, nextMeta)) {
      chatViewMetaRef.current = nextMeta;
      setChatViewMeta(nextMeta);
    }
  }, [pendingQuestion]);

  useEffect(() => {
    pendingQuestionErrorRef.current = pendingQuestionError;
  }, [pendingQuestionError]);

  useEffect(() => {
    contextUsageRef.current = contextUsage;
  }, [contextUsage]);

  useEffect(() => {
    autoContinueAfterResumeWaitRef.current = autoContinueAfterResumeWait;
  }, [autoContinueAfterResumeWait]);

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

  useEffect(
    () => () => {
      if (assistantTurnUpdateFrameRef.current !== null) {
        window.cancelAnimationFrame(assistantTurnUpdateFrameRef.current);
        assistantTurnUpdateFrameRef.current = null;
      }
      pendingAssistantTurnUpdatesRef.current.clear();
    },
    [],
  );

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
  const modelRuntimeSummary = useMemo(() => {
    const model = modelRuntimeDraft.model.trim();
    const providerId = modelRuntimeDraft.modelProviderId.trim();
    if (!model && !providerId) {
      return "当前未配置全局模型";
    }
    if (model && providerId) {
      return `${model} · ${providerId}`;
    }
    return model || providerId;
  }, [modelRuntimeDraft]);
  const activeModelIndex = useMemo(
    () => selectableModels.findIndex((configured) =>
      configured.providerId === modelRuntimeDraft.modelProviderId
      && configured.model === modelRuntimeDraft.model,
    ),
    [modelRuntimeDraft.model, modelRuntimeDraft.modelProviderId, selectableModels],
  );
  const activeSelectableModel = activeModelIndex >= 0 ? selectableModels[activeModelIndex] : null;
  const applyGlobalRuntimeConfig = useCallback(
    (config: Awaited<ReturnType<typeof getAgentRuntimeConfig>>) => {
      setModelRuntimeDraft(buildModelRuntimeDraft(config));
      setSelectableModels(config.selectableModels ?? []);
      setRuntimeConfigError("");
    },
    [],
  );
  const refreshGlobalRuntimeConfig = useCallback(async () => {
    try {
      applyGlobalRuntimeConfig(await getAgentRuntimeConfig());
    } catch (error) {
      setRuntimeConfigError(formatRuntimeModelError({
        message: error instanceof Error ? error.message : String(error),
      }));
    }
  }, [applyGlobalRuntimeConfig]);
  useEffect(() => {
    void refreshGlobalRuntimeConfig();
  }, [refreshGlobalRuntimeConfig, runtimeConfigRevision]);
  const selectGlobalModel = useCallback(async (configured: SelectableModel) => {
    try {
      applyGlobalRuntimeConfig(await setAgentRuntimeConfig({
        modelProviderId: configured.providerId,
        model: configured.model,
      }));
    } catch (error) {
      setRuntimeConfigError(formatRuntimeModelError({
        message: error instanceof Error ? error.message : String(error),
      }));
    }
  }, [applyGlobalRuntimeConfig]);
  const reasoningEfforts = activeSelectableModel?.modelThinkingModes ?? [];
  const reasoningEffort = useMemo(() => {
    const effort = modelRuntimeDraft.modelThinkingMode.trim().toLowerCase();
    return reasoningEfforts.find(
      (candidate) => candidate === effort,
    ) ?? reasoningEfforts[0] ?? null;
  }, [modelRuntimeDraft.modelThinkingMode, reasoningEfforts]);
  const selectReasoningEffort = useCallback(async (
    effort: ModelThinkingMode,
  ) => {
    try {
      setRuntimeConfigError("");
      applyGlobalRuntimeConfig(await setAgentRuntimeConfig({
        modelThinkingMode: effort,
      }));
    } catch (error) {
      setRuntimeConfigError(formatRuntimeModelError({
        message: error instanceof Error ? error.message : String(error),
      }));
    }
  }, [applyGlobalRuntimeConfig]);
  const refreshContextUsage = useCallback(async (sessionId: string) => {
    const normalized = sessionId.trim();
    if (!normalized) {
      setContextUsage(null);
      return;
    }
    try {
      const next = await getAgentContextUsage(normalized);
      setContextUsage((current) => {
        const currentUpdatedAt = current?.updatedAt;
        const nextUpdatedAt = next.updatedAt;
        return typeof currentUpdatedAt === "number" &&
          (typeof nextUpdatedAt !== "number" || currentUpdatedAt > nextUpdatedAt)
          ? current
          : next;
      });
    } catch {
      // A transient state read must not erase the last canonical request boundary.
    }
  }, []);
  const handleCompactContext = useCallback(async () => {
    if (!currentSession || isStreaming || isManualCompacting) {
      return;
    }
    setRuntimeConfigError("");
    setIsManualCompacting(true);
    try {
      const result = await compactAgentContext(currentSession.id);
      if (!result.compacted) {
        setRuntimeConfigError("Not enough conversation history to compact.");
      }
      await refreshContextUsage(currentSession.id);
    } catch (error) {
      setRuntimeConfigError(`Compaction failed: ${formatExecutionError(error)}`);
    } finally {
      setIsManualCompacting(false);
    }
  }, [currentSession, isManualCompacting, isStreaming, refreshContextUsage]);
  const rememberReplayPayloads = useCallback(
    (payloads: readonly AgentStreamPayload[]) => {
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
    [],
  );
  const persistVisibleSessionViewCache = useCallback(
    (sessionId: string = visibleSessionIdRef.current) => {
      const normalizedSessionId = sessionId.trim();
      if (
        !normalizedSessionId ||
        normalizedSessionId !== visibleSessionIdRef.current
      ) {
        return;
      }
      const hasViewState =
        messagesRef.current.length > 0 ||
        Boolean(pendingQuestionRef.current) ||
        Object.keys(replayCursorsByAgentRunIdRef.current).length > 0;
      if (!hasViewState) {
        return;
      }
      const activeStream = activeStreamRef.current;
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
          contextUsage: contextUsageRef.current,
          autoContinueAfterResumeWait: autoContinueAfterResumeWaitRef.current,
          pendingQuestion: pendingQuestionRef.current,
          pendingQuestionError: pendingQuestionErrorRef.current,
          activeReplay,
        },
        replayCursorsByAgentRunId: replayCursorsByAgentRunIdRef.current,
        verifiedReplayAgentRunIds: Array.from(verifiedReplayAgentRunIdsRef.current),
      });
    },
    [],
  );
  const scheduleVisibleSessionViewCachePersist = useCallback(() => {
    if (sessionViewCachePersistTimerRef.current !== null) {
      return;
    }
    sessionViewCachePersistTimerRef.current = window.setTimeout(() => {
      sessionViewCachePersistTimerRef.current = null;
      persistVisibleSessionViewCache();
    }, SESSION_VIEW_CACHE_WRITE_DELAY_MS);
  }, [persistVisibleSessionViewCache]);
  scheduleVisibleSessionViewCachePersistRef.current =
    scheduleVisibleSessionViewCachePersist;
  useEffect(() => {
    scheduleVisibleSessionViewCachePersist();
  }, [
    autoContinueAfterResumeWait,
    contextUsage,
    currentSessionId,
    pendingQuestion,
    pendingQuestionError,
    scheduleVisibleSessionViewCachePersist,
  ]);
  const flushAssistantTurnUpdates = useCallback(() => {
    if (assistantTurnUpdateFrameRef.current !== null) {
      window.cancelAnimationFrame(assistantTurnUpdateFrameRef.current);
      assistantTurnUpdateFrameRef.current = null;
    }
    if (pendingAssistantTurnUpdatesRef.current.size === 0) {
      return;
    }
    const pendingUpdates = pendingAssistantTurnUpdatesRef.current;
    pendingAssistantTurnUpdatesRef.current = new Map();
    const previous = messagesRef.current;
    let changed = false;
    let next: ChatMessage[] | null = null;
    let refreshMeta = false;
    const changedAssistantMessages: AssistantChatMessage[] = [];
    for (const [messageId, updaters] of pendingUpdates.entries()) {
      if (updaters.length === 0) {
        continue;
      }
      const messageIndex = messageIndexByIdRef.current.get(messageId);
      if (messageIndex === undefined) {
        continue;
      }
      const item = previous[messageIndex];
      if (!item || item.role !== "assistant" || item.id !== messageId) {
        continue;
      }
      let nextTurn = item.turn;
      let pendingTextDeltas: string[] = [];
      const flushTextDeltas = () => {
        if (pendingTextDeltas.length === 0) {
          return;
        }
        nextTurn = {
          ...nextTurn,
          finalAnswer: `${nextTurn.finalAnswer}${pendingTextDeltas.join("")}`,
          activity: null,
        };
        pendingTextDeltas = [];
      };
      for (const update of updaters) {
        if (update.kind === "textDelta") {
          pendingTextDeltas.push(update.delta);
          continue;
        }
        flushTextDeltas();
        nextTurn = update.updater(nextTurn);
        refreshMeta = true;
      }
      flushTextDeltas();
      if (nextTurn === item.turn) {
        continue;
      }
      if (!next) {
        next = [...previous];
      }
      changed = true;
      const nextMessage: AssistantChatMessage = {
        ...item,
        turn: nextTurn,
      };
      next[messageIndex] = nextMessage;
      changedAssistantMessages.push(nextMessage);
    }
    if (!changed) {
      return;
    }
    commitMessagesToView(next ?? previous, {
      assistantMessages: changedAssistantMessages,
      refreshMeta,
    });
  }, [commitMessagesToView]);

  const scheduleAssistantTurnFlush = useCallback(() => {
    if (assistantTurnUpdateFrameRef.current !== null) {
      return;
    }
    assistantTurnUpdateFrameRef.current = window.requestAnimationFrame(() => {
      assistantTurnUpdateFrameRef.current = null;
      flushAssistantTurnUpdates();
    });
  }, [flushAssistantTurnUpdates]);

  const updateAssistantTurn = useCallback(
    (messageId: string, updater: AssistantTurnUpdater) => {
      const normalizedMessageId = messageId.trim();
      if (!normalizedMessageId) {
        throw new Error("assistant message id is required for turn update");
      }
      const pendingUpdates = pendingAssistantTurnUpdatesRef.current;
      const updaters = pendingUpdates.get(normalizedMessageId);
      const pendingUpdate: PendingAssistantTurnUpdate = {
        kind: "update",
        updater,
      };
      if (updaters) {
        updaters.push(pendingUpdate);
      } else {
        pendingUpdates.set(normalizedMessageId, [pendingUpdate]);
      }
      scheduleAssistantTurnFlush();
    },
    [scheduleAssistantTurnFlush],
  );

  const appendAssistantTextDelta = useCallback(
    (messageId: string, delta: string) => {
      if (!delta) {
        return;
      }
      const normalizedMessageId = messageId.trim();
      if (!normalizedMessageId) {
        throw new Error("assistant message id is required for text delta");
      }
      const pendingUpdates = pendingAssistantTurnUpdatesRef.current;
      const update: PendingAssistantTurnUpdate = { kind: "textDelta", delta };
      const updates = pendingUpdates.get(normalizedMessageId);
      if (updates) {
        updates.push(update);
      } else {
        pendingUpdates.set(normalizedMessageId, [update]);
      }
      scheduleAssistantTurnFlush();
    },
    [scheduleAssistantTurnFlush],
  );

  const closeActiveStream = useCallback(() => {
    const active = activeStreamRef.current;
    if (!active) {
      return;
    }
    flushAssistantTurnUpdates();
    active.close();
    activeStreamRef.current = null;
    setIsStreaming(false);
  }, [flushAssistantTurnUpdates]);

  const closeStreamForMessage = useCallback(
    (assistantMessageId: string) => {
      const active = activeStreamRef.current;
      if (!active || active.assistantMessageId !== assistantMessageId) {
        return;
      }
      flushAssistantTurnUpdates();
      closeActiveStream();
    },
    [closeActiveStream, flushAssistantTurnUpdates],
  );

  const finishAssistantStreamWithError = useCallback(
    (
      assistantMessageId: string,
      message: string,
      activity?: RuntimeActivity,
      turnId?: string,
    ) => {
      const active = activeStreamRef.current;
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
      onAgentRunningChange,
      updateAssistantTurn,
    ],
  );
  const ensureStreamSeenSets = useCallback(
    (assistantMessageId: string): StreamSeenSets => {
      const active = activeStreamRef.current;
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
    [],
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
              : activeStreamRef.current?.sessionId || "";
          setContextUsage((current) => {
            return current
              ? {
                  ...current,
                  sessionId: eventSessionId,
                  isCompacting: purpose === "compaction",
                }
              : current;
          });
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
        const delta =
          typeof payload.delta === "string" ? payload.delta : "";
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
          let nextTurn = applySessionEventToAssistantTurn(
            turn,
            event,
          );
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
        let nextTurn = applySessionEventToAssistantTurn(
          turn,
          event,
        );
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
          let terminalStatus: ReturnType<
            typeof getTerminalSessionEventStatus
          > = null;
          try {
            terminalStatus =
              payload.type === "session_event"
                ? getTerminalSessionEventStatus(event)
                : null;
          } catch (error) {
            finishAssistantStreamWithError(
              assistantMessageId,
              error instanceof Error ? error.message : String(error),
              normalizeRuntimeActivity("协议错误", "summarizing"),
            );
            return;
          }
          if (terminalStatus) {
            const active = activeStreamRef.current;
            if (
              active?.agentRunId &&
              normalizeAgentRunId(payload.agentRunId) !== active.agentRunId
            ) {
              finishAssistantStreamWithError(
                assistantMessageId,
                "协议错误：终态 session_event 的 agentRunId 与活动 AgentRun 不匹配。",
                normalizeRuntimeActivity("协议错误", "summarizing"),
              );
              return;
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
            if (
              resolvedSession &&
              activeStreamRef.current?.sessionId === resolvedSession.id
            ) {
              pendingResolvedSessionRef.current = null;
              preserveResolvedSessionIdRef.current = resolvedSession.id;
              onSessionResolved?.(resolvedSession, { activate: false });
            }
            const sessionId =
              (typeof event.sessionId === "string" && event.sessionId.trim()) ||
              active?.sessionId ||
              "";
            if (
              active?.assistantMessageId === assistantMessageId &&
              sessionId
            ) {
              onAgentRunningChange?.(sessionId, false);
              onSessionCompleted?.(sessionId);
              void refreshContextUsage(sessionId);
            }
            closeStreamForMessage(assistantMessageId);
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
      closeStreamForMessage,
      ensureStreamSeenSets,
      finishAssistantStreamWithError,
      flushAssistantTurnUpdates,
      onSessionResolved,
      onSessionCompleted,
      onAgentRunningChange,
      rememberReplayPayloads,
      refreshContextUsage,
      updateAssistantTurn,
    ],
  );
  processStreamPayloadRef.current = processStreamPayload;

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
        activeStreamRef.current?.assistantMessageId === assistantMessageId &&
        activeStreamRef.current?.agentRunId === agentRunId
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
          processStreamPayload(assistantMessageId, payload);
        },
        (error) => {
          if (
            !activeStreamRef.current ||
            activeStreamRef.current.assistantMessageId !== assistantMessageId
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
          setIsStreaming(true);
        },
      );

      activeStreamRef.current = {
        sessionId,
        agentRunId,
        assistantMessageId,
        seenSessionEvent: replaySeen.seenSessionEvent,
        seenSessionEventIds: replaySeen.seenSessionEventIds,
        close: stream.close,
      };
    },
    [
      closeActiveStream,
      finishAssistantStreamWithError,
      onAgentRunningChange,
      processStreamPayload,
      updateAssistantTurn,
    ],
  );

  const handleQuestionOptionToggle = useCallback((option: string) => {
    setPendingQuestion((previous) => {
      if (!previous) {
        return previous;
      }
      if (!previous.request.multiSelect) {
        return {
          ...previous,
          selectedOptions: [option],
        };
      }
      const hasOption = previous.selectedOptions.includes(option);
      return {
        ...previous,
        selectedOptions: hasOption
          ? previous.selectedOptions.filter((item) => item !== option)
          : [...previous.selectedOptions, option],
      };
    });
    setPendingQuestionError("");
  }, []);

  const handleQuestionTextChange = useCallback((value: string) => {
    setPendingQuestion((previous) => {
      if (!previous) {
        return previous;
      }
      return {
        ...previous,
        answerText: value,
      };
    });
    setPendingQuestionError("");
  }, []);

  const handleQuestionSubmit = useCallback(async () => {
    if (!pendingQuestion || !currentSession) {
      return;
    }

    const answerText = pendingQuestion.answerText.trim();
    const answers = pendingQuestion.selectedOptions;
    if (
      pendingQuestion.request.required &&
      answers.length === 0 &&
      !answerText
    ) {
      setPendingQuestionError("请至少提供一个回答。");
      return;
    }

    setPendingQuestionError("");
    setPendingQuestion((previous) => {
      if (!previous) {
        return previous;
      }
      return {
        ...previous,
        submitting: true,
      };
    });

    const now = Date.now();
    const userSummary =
      answers.length > 0
        ? `回答：${answers.join("；")}${answerText ? `；${answerText}` : ""}`
        : `回答：${answerText || "已提交"}`;
    const userMessage: ChatMessage = {
      id: `user-question-${now}`,
      role: "user",
      text: userSummary,
      timestamp: now,
    };
    const assistantMessage: ChatMessage = {
      id: `assistant-question-${now}`,
      role: "assistant",
      turn: buildPendingTurn(),
    };
    setMessages((previous) => [...previous, userMessage, assistantMessage]);

    try {
      const response = await answerAgentQuestion({
        sessionId: currentSession.id,
        questionId: pendingQuestion.request.id,
        answers: answers.length > 0 ? answers : undefined,
        answerText: answerText || undefined,
        autoContinueAfterResumeWait,
      });
      const responseSessionId =
        typeof response.sessionId === "string" &&
          response.sessionId.trim().length > 0
          ? response.sessionId.trim()
          : currentSession.id;
      const responseAgentRunId =
        typeof response.agentRunId === "string" && response.agentRunId.trim().length > 0
          ? response.agentRunId.trim()
          : "";
      if (!responseAgentRunId) throw new Error("missing agentRunId");
      setPendingQuestion(null);
      startStreamForAssistant(
        assistantMessage.id,
        responseSessionId,
        responseAgentRunId,
        [],
      );
      void refreshContextUsage(responseSessionId);
    } catch {
      onAgentRunningChange?.(currentSession.id, false);
      updateAssistantTurn(assistantMessage.id, (turn) => {
        const withError = appendNarrativeChunk(
          turn,
          "回答提交失败，请稍后重试。",
          "error",
        );
        return {
          ...withError,
          isStreaming: false,
          activity: undefined,
        };
      });
      setPendingQuestion((previous) => {
        if (!previous) {
          return previous;
        }
        return {
          ...previous,
          submitting: false,
        };
      });
    }
  }, [
      autoContinueAfterResumeWait,
      activeWorkspaceRoot,
      currentSession,
    onSessionCompleted,
    onAgentRunningChange,
    pendingQuestion,
    refreshContextUsage,
    startStreamForAssistant,
    updateAssistantTurn,
  ]);

  const applySessionViewSnapshot = useCallback(
    (sessionId: string, snapshot: SessionViewSnapshot) => {
      visibleSessionIdRef.current = sessionId;
      visibleActiveReplayRef.current = snapshot.activeReplay;
      setAutoContinueAfterResumeWait(snapshot.autoContinueAfterResumeWait);
      setContextUsage(snapshot.contextUsage);
      setPendingQuestion(snapshot.pendingQuestion);
      setPendingQuestionError(snapshot.pendingQuestionError);
      setMessages(snapshot.messages);
      setIsStreaming(Boolean(snapshot.activeReplay));
    },
    [],
  );

  const cachedSnapshotHasToolProcess = useCallback(
    (snapshot: SessionViewSnapshot): boolean =>
      snapshot.messages.some((message) => {
        if (message.role !== "assistant") {
          return false;
        }
        return message.turn.chunks.some(
          (chunk) => chunk.kind === "task" || chunk.kind === "subagent",
        );
      }),
    [],
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
          agentRun.completedAtMs ?? agentRun.updatedAtMs ?? turn.completedAtMs ?? Date.now(),
      }));
    },
    [updateAssistantTurn],
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
      replayCursorsByAgentRunIdRef.current = { ...snapshot.replayCursorsByAgentRunId };
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
      setContextUsage(snapshot.contextUsage);
      setHydrationStage("applySnapshot");
      setMessages(hydratedMessages);
      setIsHydratingSession(false);
      setHydrationStage("");
      if (snapshot.pendingQuestionRequest && snapshot.restoreMessageId) {
        setPendingQuestion({
          assistantMessageId: snapshot.restoreMessageId,
          request: snapshot.pendingQuestionRequest,
          selectedOptions: [],
          answerText: "",
          submitting: false,
        });
      }
      sessionViewCacheStore.write({
        sessionId,
        snapshot: {
          messages: hydratedMessages,
          contextUsage: snapshot.contextUsage,
          autoContinueAfterResumeWait:
            snapshot.resolvedAutoContinueAfterResumeWait,
          pendingQuestion:
            snapshot.pendingQuestionRequest && snapshot.restoreMessageId
              ? {
                assistantMessageId: snapshot.restoreMessageId,
                request: snapshot.pendingQuestionRequest,
                selectedOptions: [],
                answerText: "",
                submitting: false,
              }
              : null,
          pendingQuestionError: "",
          activeReplay: snapshot.activeReplay
            ? {
              messageId: snapshot.activeReplay.messageId,
              agentRunId: snapshot.activeReplay.agentRunId,
            }
            : null,
        },
        replayCursorsByAgentRunId: snapshot.replayCursorsByAgentRunId,
        verifiedReplayAgentRunIds: Object.keys(snapshot.replayCursorsByAgentRunId),
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
    [applyGlobalRuntimeConfig, startStreamForAssistant],
  );

  const refreshCachedSessionFromDurableLog = useCallback(
    async (
      sessionId: string,
      cachedEntry: SessionViewCacheEntry<SessionViewSnapshot>,
      isLatestHydrate: () => boolean,
    ) => {
      const hydrationControl = {
        isCancelled: () => !isLatestHydrate(),
        yieldToUi: waitForNextPaint,
        onStage: (stage: string) => {
          if (isLatestHydrate()) {
            setHydrationStage(stage);
          }
        },
      };
      setHydrationStage("refreshCachedSession");
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
      if (!isLatestHydrate()) {
        return;
      }
      const agentRuns = Array.isArray(taskResponse.agentRuns) ? taskResponse.agentRuns : [];
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
        cachedMessageIds: messagesRef.current.map((message) => message.id),
        cachedReplayCursorsByAgentRunId: cachedEntry.replayCursorsByAgentRunId,
        cachedVerifiedReplayAgentRunIds: cachedEntry.verifiedReplayAgentRunIds,
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
        if (!isLatestHydrate()) {
          return;
        }
        applyHydrationSnapshot(snapshot, sessionId);
        return;
      }
      const replayRun = selectReplayAgentRun(agentRuns);
      const agentRunIds = new Set(
        Object.keys(cachedEntry.replayCursorsByAgentRunId).map(normalizeAgentRunId),
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
      setHydrationStage("fetchDeltaReplays");
      const deltaEntries = await Promise.all(
        normalizedAgentRunIds.map(async (agentRunId) => {
          const cursor = cachedEntry.replayCursorsByAgentRunId[agentRunId] ?? 0;
          const snapshot = await replayAgentRunStreamFromCursor(agentRunId, cursor);
          return [agentRunId, snapshot] as const;
        }),
      );
      if (!isLatestHydrate()) {
        return;
      }
      setHydrationStage("applyDeltaReplays");
      let cursorPatch: SessionReplayCursors = {};
      let appliedDeltaPayloads = 0;
      for (const [agentRunId, snapshot] of deltaEntries) {
        if (!isLatestHydrate()) {
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
          if (!isLatestHydrate()) {
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
            if (!isLatestHydrate()) {
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
          deltaEntries.find(([entryAgentRunId]) => entryAgentRunId === agentRunId)?.[1]
            .items ?? [];
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
      cachedSnapshotHasToolProcess,
      flushAssistantTurnUpdates,
      markAgentRunTerminalInView,
      persistVisibleSessionViewCache,
      processStreamPayload,
      startStreamForAssistant,
    ],
  );

  useEffect(() => {
    setSessionLoadError("");
    if (
      currentSessionId &&
      preserveResolvedSessionIdRef.current === currentSessionId &&
      messagesRef.current.length > 0
    ) {
      preserveResolvedSessionIdRef.current = null;
      visibleSessionIdRef.current = currentSessionId;
      visibleActiveReplayRef.current = activeStreamRef.current?.agentRunId
        ? {
            messageId: activeStreamRef.current.assistantMessageId,
            agentRunId: activeStreamRef.current.agentRunId,
          }
        : visibleActiveReplayRef.current;
      setIsStreaming(false);
      return;
    }
    closeActiveStream();
    pendingResolvedSessionRef.current = null;
    const cachedEntry = currentSessionId
      ? sessionViewCacheStore.get(currentSessionId)
      : null;
    setEditingUserMessageId(null);
    setEditingPrompt("");
    hydrateRequestIdRef.current += 1;
    if (!currentSessionId) {
      visibleSessionIdRef.current = "";
      visibleActiveReplayRef.current = null;
      replayCursorsByAgentRunIdRef.current = {};
      verifiedReplayAgentRunIdsRef.current.clear();
      setPendingQuestion(null);
      setPendingQuestionError("");
      setContextUsage(null);
      setIsStreaming(false);
      setIsHydratingSession(false);
      setHydrationStage("");
      setMessages([]);
      return;
    }

    let cancelled = false;
    const hydrateRequestId = hydrateRequestIdRef.current;
    const isLatestHydrate = () =>
      !cancelled && hydrateRequestIdRef.current === hydrateRequestId;
    if (cachedEntry) {
      visibleSessionIdRef.current = currentSessionId;
      replayCursorsByAgentRunIdRef.current = {
        ...cachedEntry.replayCursorsByAgentRunId,
      };
      verifiedReplayAgentRunIdsRef.current = new Set(
        cachedEntry.verifiedReplayAgentRunIds,
      );
      applySessionViewSnapshot(currentSessionId, cachedEntry.snapshot);
      setIsHydratingSession(false);
    } else {
      visibleSessionIdRef.current = currentSessionId;
      visibleActiveReplayRef.current = null;
      replayCursorsByAgentRunIdRef.current = {};
      verifiedReplayAgentRunIdsRef.current.clear();
      streamSeenByMessageIdRef.current.clear();
      setPendingQuestion(null);
      setPendingQuestionError("");
      setContextUsage(null);
      setIsStreaming(false);
      setMessages([]);
      setHydrationStage("fetchProjection");
      setIsHydratingSession(true);
    }

    const hydrateSession = async () => {
      try {
        if (cachedEntry) {
          await refreshCachedSessionFromDurableLog(
            currentSessionId,
            cachedEntry,
            isLatestHydrate,
          );
          return;
        }
        const snapshot = await buildSessionHydrationSnapshot(currentSessionId, {
          isCancelled: () => !isLatestHydrate(),
          yieldToUi: waitForNextPaint,
          onStage: (stage: string) => {
            if (isLatestHydrate()) {
              setHydrationStage(stage);
            }
          },
        });
        if (!isLatestHydrate()) {
          return;
        }
        applyHydrationSnapshot(snapshot, currentSessionId);
      } catch (error) {
        if (!isLatestHydrate()) {
          return;
        }
        sessionViewCacheStore.delete(currentSessionId);
        visibleActiveReplayRef.current = null;
        replayCursorsByAgentRunIdRef.current = {};
        verifiedReplayAgentRunIdsRef.current.clear();
        setMessages([]);
        setPendingQuestion(null);
        setPendingQuestionError("");
        setContextUsage(null);
        setIsStreaming(false);
        setIsHydratingSession(false);
        setHydrationStage("");
        setSessionLoadError(formatExecutionError(error));
      }
    };

    void hydrateSession();
    return () => {
      cancelled = true;
      if (hydrateRequestIdRef.current === hydrateRequestId) {
        setIsHydratingSession(false);
        setHydrationStage("");
      }
    };
  }, [
    applyHydrationSnapshot,
    applySessionViewSnapshot,
    closeActiveStream,
    currentSessionId,
    refreshCachedSessionFromDurableLog,
  ]);

  useEffect(() => {
    return () => {
      closeActiveStream();
      if (copiedUserMessageTimeoutRef.current !== null) {
        window.clearTimeout(copiedUserMessageTimeoutRef.current);
        copiedUserMessageTimeoutRef.current = null;
      }
    };
  }, [closeActiveStream]);

  const handleMessagesScroll = useCallback(() => {
    const container = messagesContainerRef.current;
    if (!container) {
      return;
    }
    setFollowingLatest(isNearScrollBottom(container));
  }, [setFollowingLatest]);

  const handleJumpToLatest = useCallback(() => {
    setFollowingLatest(true);
    scheduleFollowLatestScroll();
  }, [scheduleFollowLatestScroll, setFollowingLatest]);

  useEffect(() => {
    return () => {
      if (followLatestFrameRef.current !== null) {
        window.cancelAnimationFrame(followLatestFrameRef.current);
        followLatestFrameRef.current = null;
      }
    };
  }, []);

  useEffect(() => {
    return () => {
      if (sessionViewCachePersistTimerRef.current !== null) {
        window.clearTimeout(sessionViewCachePersistTimerRef.current);
        sessionViewCachePersistTimerRef.current = null;
      }
    };
  }, []);

  const handleStopActiveAgentRun = useCallback(() => {
    const active = activeStreamRef.current;
    if (!active) {
      return;
    }
    stoppedAssistantMessageIdsRef.current.add(active.assistantMessageId);
    const now = Date.now();
    updateAssistantTurn(active.assistantMessageId, (turn) => {
      return {
        ...turn,
        isStreaming: false,
        activity: undefined,
        completedAtMs: now,
      };
    });
    flushAssistantTurnUpdates();
    visibleActiveReplayRef.current = null;
    onAgentRunningChange?.(active.sessionId, false);
    const queuedPrompt =
      queuedNextPromptSessionIdRef.current === active.sessionId
        ? queuedNextPromptRef.current
        : "";
    setQueuedNextPromptText("");
    setInputValue(recoverQueuedPromptAfterStop(queuedPrompt, inputValue));
    if (isNativeHostRuntime()) {
      void (async () => {
        await cancelAgentRun({
          agentRunId: active.agentRunId,
          sessionId: active.sessionId,
          reason: "user_interrupt",
        });
      })().catch(() => {
        updateAssistantTurn(active.assistantMessageId, (turn) =>
          appendNarrativeChunk(turn, "停止请求未能写入后台任务状态。", "error"),
        );
      });
    }
    closeActiveStream();
  }, [
    closeActiveStream,
    flushAssistantTurnUpdates,
    inputValue,
    onAgentRunningChange,
    setQueuedNextPromptText,
    updateAssistantTurn,
  ]);

  const applyDurableTurnMessageIds = useCallback(
    (
      temporaryUserMessageId: string,
      temporaryAssistantMessageId: string,
      turnIdValue: string | undefined,
    ): { userMessageId: string; assistantMessageId: string } => {
      const turnId = typeof turnIdValue === "string" ? turnIdValue.trim() : "";
      if (!turnId) {
        return {
          userMessageId: temporaryUserMessageId,
          assistantMessageId: temporaryAssistantMessageId,
        };
      }
      const userMessageId = `msg:user:${turnId}`;
      const assistantMessageId = `msg:assistant:${turnId}`;
      if (
        userMessageId === temporaryUserMessageId &&
        assistantMessageId === temporaryAssistantMessageId
      ) {
        return { userMessageId, assistantMessageId };
      }
      setMessages((previous) =>
        previous.map((item) => {
          if (item.id === temporaryUserMessageId && item.role === "user") {
            return {
              ...item,
              id: userMessageId,
            };
          }
          if (
            item.id === temporaryAssistantMessageId &&
            item.role === "assistant"
          ) {
            return {
              ...item,
              id: assistantMessageId,
            };
          }
          return item;
        }),
      );
      if (
        activeStreamRef.current?.assistantMessageId ===
        temporaryAssistantMessageId
      ) {
        activeStreamRef.current.assistantMessageId = assistantMessageId;
      }
      if (stoppedAssistantMessageIdsRef.current.delete(temporaryAssistantMessageId)) {
        stoppedAssistantMessageIdsRef.current.add(assistantMessageId);
      }
      return { userMessageId, assistantMessageId };
    },
    [],
  );

  const sendPromptAsSupplement = async (
    prompt: string,
    activeStream: ActiveStreamState,
  ) => {
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
        throw new Error("_centaeris/session/supplement rejected supplement input");
      }
      const expectedAgentRunId = normalizeAgentRunId(activeStream.agentRunId);
      const responseAgentRunId = normalizeAgentRunId(response.agentRunId);
      const responseSessionId = typeof response.sessionId === "string" ? response.sessionId.trim() : "";
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
  };

  const handleSend = async (queuedPrompt?: string) => {
    const prompt = (queuedPrompt ?? inputValue).trim();
    if (!prompt) {
      return;
    }
    const activeStream = activeStreamRef.current;
    if (activeStream) {
      if (queuedPrompt === undefined) {
        handleJumpToLatest();
      }
      setQueuedNextPromptText(prompt, activeStream.sessionId);
      if (queuedPrompt === undefined) {
        setInputValue("");
      }
      return;
    }
    if (queuedPrompt === undefined) {
      setFollowingLatest(true);
    }

    let targetSession = currentSession;
    if (!targetSession) {
      try {
        if (!activeWorkspaceRoot) {
          throw new Error("请先选择真实工作区，再开始本地会话");
        }
        const created = await createSession(prompt, activeWorkspaceRoot);
        targetSession = {
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
        };
      } catch (error) {
        const now = Date.now();
        const userMessage: ChatMessage = {
          id: `user-${now}`,
          role: "user",
          text: prompt,
          timestamp: now,
        };
        const assistantMessage: ChatMessage = {
          id: `assistant-${now}-${Math.random().toString(36).slice(2, 6)}`,
          role: "assistant",
          turn: {
            ...buildPendingTurn(),
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
        };
        setMessages((previous) => [
          ...prunePendingTailMessages(previous),
          userMessage,
          assistantMessage,
        ]);
        setInputValue("");
        return;
      }
    }

    const now = Date.now();
    const userMessage: ChatMessage = {
      id: `user-${now}`,
      role: "user",
      text: prompt,
      timestamp: now,
    };

    const assistantTurn = buildPendingTurn();
    const assistantMessage: ChatMessage = {
      id: `assistant-${now}-${Math.random().toString(36).slice(2, 6)}`,
      role: "assistant",
      turn: assistantTurn,
    };

    visibleSessionIdRef.current = targetSession.id;
    setEditingUserMessageId(null);
    setEditingPrompt("");
    setMessages((previous) => [
      ...prunePendingTailMessages(previous),
      userMessage,
      assistantMessage,
    ]);
    setInputValue("");
    if (!currentSession && targetSession) {
      preserveResolvedSessionIdRef.current = targetSession.id;
      onSessionResolved?.(targetSession, { activate: true });
    }

    try {
      await waitForNextPaint();
      const response = await sendAgentInput({
        sessionId: targetSession.id,
        message: prompt,
        preferredLocale: "zh-CN",
        autoContinueAfterResumeWait,
      });
      const responseSessionId =
        typeof response.sessionId === "string" &&
          response.sessionId.trim().length > 0
          ? response.sessionId.trim()
          : targetSession.id;
      const responseAgentRunId =
        typeof response.agentRunId === "string" && response.agentRunId.trim().length > 0
          ? response.agentRunId.trim()
          : "";
      if (!responseAgentRunId) throw new Error("missing agentRunId");
      const durableIds = applyDurableTurnMessageIds(
        userMessage.id,
        assistantMessage.id,
        response.turnId,
      );
      if (
        stoppedAssistantMessageIdsRef.current.delete(
          durableIds.assistantMessageId,
        )
      ) {
        onAgentRunningChange?.(responseSessionId, false);
        void refreshContextUsage(responseSessionId);
        return;
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
    } catch (error) {
      onAgentRunningChange?.(targetSession.id, false);
      updateAssistantTurn(assistantMessage.id, (turn) => {
        const withError = appendNarrativeChunk(
          turn,
          formatExecutionError(error),
          "error",
        );
        return {
          ...withError,
          isStreaming: false,
          activity: undefined,
        };
      });
      closeActiveStream();
    }
  };

  useEffect(() => {
    if (isStreaming || activeStreamRef.current) {
      return;
    }
    const prompt = queuedNextPromptRef.current.trim();
    if (
      !prompt ||
      queuedNextPromptSessionIdRef.current !== currentSessionId
    ) {
      return;
    }
    setQueuedNextPromptText("");
    void handleSend(prompt);
  }, [currentSessionId, isStreaming]);

  const handleComposerAction = () => {
    if (hasInput) {
      void handleSend();
      return;
    }
    if (isStreaming) {
      handleStopActiveAgentRun();
    }
  };

  const handleEditQueuedNextPrompt = () => {
    const prompt = queuedNextPromptRef.current;
    setQueuedNextPromptText("");
    setInputValue(prompt);
  };

  const handleSubmitQueuedNextPromptAsSupplement = () => {
    const prompt = queuedNextPromptRef.current.trim();
    if (!prompt) {
      return;
    }
    setQueuedNextPromptText("");
    const activeStream = activeStreamRef.current;
    if (activeStream) {
      void sendPromptAsSupplement(prompt, activeStream);
      return;
    }
    void handleSend(prompt);
  };

  const handleStartEditingUserMessage = (
    message: Extract<ChatMessage, { role: "user" }>,
  ) => {
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
  };

  const handleCancelEditingUserMessage = () => {
    setEditingUserMessageId(null);
    setEditingPrompt("");
  };

  const handleCopyUserMessage = async (messageId: string, text: string) => {
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
  };

  const handleSubmitEditedUserMessage = async (messageId: string) => {
    const prompt = editingPrompt.trim();
    if (!prompt || isStreaming || activeStreamRef.current) {
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

    let targetSession = currentSession;
    if (!targetSession) {
      try {
        if (!activeWorkspaceRoot) {
          throw new Error("请先选择真实工作区，再开始本地会话");
        }
        const created = await createSession(prompt, activeWorkspaceRoot);
        targetSession = {
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
        };
      } catch (error) {
        const now = Date.now();
        const userMessage: ChatMessage = {
          id: `user-${now}`,
          role: "user",
          text: prompt,
          timestamp: now,
        };
        const assistantMessage: ChatMessage = {
          id: `assistant-${now}-${Math.random().toString(36).slice(2, 6)}`,
          role: "assistant",
          turn: {
            ...buildPendingTurn(),
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
        };
        setMessages((previous) => {
          const index = previous.findIndex((item) => item.id === messageId);
          if (index < 0) {
            return [
              ...prunePendingTailMessages(previous),
              userMessage,
              assistantMessage,
            ];
          }
          return [...previous.slice(0, index), userMessage, assistantMessage];
        });
        setEditingUserMessageId(null);
        setEditingPrompt("");
        return;
      }
    }

    const now = Date.now();
    const userMessage: ChatMessage = {
      id: `user-${now}`,
      role: "user",
      text: prompt,
      timestamp: now,
    };
    const assistantMessage: ChatMessage = {
      id: `assistant-${now}-${Math.random().toString(36).slice(2, 6)}`,
      role: "assistant",
      turn: buildPendingTurn(),
    };

    setMessages((previous) => {
      const index = previous.findIndex((item) => item.id === messageId);
      if (index < 0) {
        return [
          ...prunePendingTailMessages(previous),
          userMessage,
          assistantMessage,
        ];
      }
      return [...previous.slice(0, index), userMessage, assistantMessage];
    });
    setEditingUserMessageId(null);
    setEditingPrompt("");
    if (!currentSession && targetSession) {
      preserveResolvedSessionIdRef.current = targetSession.id;
      onSessionResolved?.(targetSession, { activate: true });
    }

    let inputAccepted = false;
    try {
      await waitForNextPaint();
      const response = await sendAgentInput({
        sessionId: targetSession.id,
        message: prompt,
        preferredLocale: "zh-CN",
        autoContinueAfterResumeWait,
        tailPolicy: "rewriteLastUser",
        rewriteTargetMessageId: messageId,
        rewriteExpectedTailMessageId: expectedTailMessageId,
      });
      inputAccepted = true;
      const responseSessionId =
        typeof response.sessionId === "string" &&
          response.sessionId.trim().length > 0
          ? response.sessionId.trim()
          : targetSession.id;
      const responseAgentRunId =
        typeof response.agentRunId === "string" && response.agentRunId.trim().length > 0
          ? response.agentRunId.trim()
          : "";
      if (!responseAgentRunId) throw new Error("missing agentRunId");
      const durableIds = applyDurableTurnMessageIds(
        userMessage.id,
        assistantMessage.id,
        response.turnId,
      );
      if (
        stoppedAssistantMessageIdsRef.current.delete(
          durableIds.assistantMessageId,
        )
      ) {
        onAgentRunningChange?.(responseSessionId, false);
        void refreshContextUsage(responseSessionId);
        return;
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
    } catch (error) {
      onAgentRunningChange?.(targetSession.id, false);
      if (!inputAccepted) {
        closeActiveStream();
        setMessages(currentMessages);
        setEditingUserMessageId(messageId);
        setEditingPrompt(prompt);
        setRuntimeConfigError(`编辑失败：${formatExecutionError(error)}`);
        return;
      }
      updateAssistantTurn(assistantMessage.id, (turn) => {
        const withError = appendNarrativeChunk(
          turn,
          formatExecutionError(error),
          "error",
        );
        return {
          ...withError,
          isStreaming: false,
          activity: undefined,
        };
      });
      closeActiveStream();
    }
  };

  return (
    <div
      className={`chat-container ${chatViewMode !== "welcome" ? "has-conversation" : "is-empty"}`}
      style={chatContainerStyle}
    >
      {isPinnedSummaryOpen && shouldShowPinnedSummary ? (
        <section
          className={`pinnedSummaryCard ${isPinnedSummaryRetracting ? "is-retracting" : ""}`}
          aria-label="摘要/状态"
        >
          <header className="pinnedSummaryHeader">
            <span>环境信息</span>
          </header>
          <div className="pinnedSummaryRows">
            <div className="pinnedSummaryRow">
              <HardDrive className="pinnedSummaryIcon" aria-hidden="true" />
              <span>本地</span>
              <strong>{shouldShowWorkspaceLabel ? workspaceName : "未绑定工作区"}</strong>
            </div>
            {gitStatus ? (
              <>
                <div className="pinnedSummaryRow">
                  <FileText className="pinnedSummaryIcon" aria-hidden="true" />
                  <span>变更</span>
                  <strong className="pinnedSummaryDiff">
                    <span className="is-added">+{gitStatus.totalAdded.toLocaleString()}</span>
                    <span className="is-removed">-{gitStatus.totalRemoved.toLocaleString()}</span>
                  </strong>
                </div>
                <div className="pinnedSummaryRow">
                  <GitBranch className="pinnedSummaryIcon" aria-hidden="true" />
                  <span>分支</span>
                  <strong>{gitStatus.branch || "detached"}</strong>
                </div>
              </>
            ) : (
              <div className="pinnedSummaryRow">
                <GitBranch className="pinnedSummaryIcon" aria-hidden="true" />
                <span>Git</span>
                <strong>{gitStatusError || "Git 状态未连接"}</strong>
              </div>
            )}
            <div className="pinnedSummaryRow">
              <CloudUpload className="pinnedSummaryIcon" aria-hidden="true" />
              <span>提交或推送</span>
              <strong>未连接</strong>
            </div>
            <div className="pinnedSummaryRow">
              <GitHubMarkIcon className="pinnedSummaryIcon" aria-hidden="true" />
              <span>GitHub CLI</span>
              <strong>{githubCliStatus?.summary ?? "GitHub CLI 未检测"}</strong>
            </div>
          </div>
          <div className="pinnedSummaryDivider" />
          <div className="pinnedSummarySource">
            <span>来源</span>
            <p>暂无来源</p>
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
              <h2>无法加载会话</h2>
              <p>{sessionLoadError}</p>
            </section>
          ) : chatViewMode === "welcome" ? null : chatViewMode === "restoring" ? (
            <div
              className="conversation-restore-section"
              data-chat-view-mode="restoring"
            >
              <span>
                {isHydratingSession
                  ? `正在恢复会话${hydrationStage ? `：${HYDRATION_STAGE_LABELS[hydrationStage] || hydrationStage}` : ""}...`
                  : "当前会话暂无消息"}
              </span>
            </div>
          ) : (
            <VirtualMessageList
              containerRef={messagesContainerRef}
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
              onSubmitEditedUserMessage={(messageId) =>
                void handleSubmitEditedUserMessage(messageId)
              }
              onCopyUserMessage={(messageId, text) =>
                void handleCopyUserMessage(messageId, text)
              }
              onStartEditingUserMessage={handleStartEditingUserMessage}
              onOpenWorkspacePath={onOpenWorkspacePath}
            />
          )}
        </div>
        {!sessionLoadError && !isFollowingLatest && chatViewMode === "conversation" ? (
          <button
            type="button"
            className="jump-to-latest"
            onClick={handleJumpToLatest}
            aria-label="回到最新"
            title="回到最新"
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
                onSubmit={() => void handleQuestionSubmit()}
              />
            ) : null}

            {queuedNextPrompt.trim() ? (
              <div className="queuedNextPromptDrawer">
                <div className="queuedNextPromptText">
                  {queuedNextPrompt.trim()}
                </div>
                <div className="queuedNextPromptActions">
                  <Tooltip content="编辑">
                    <Button
                      type="button"
                      variant="composerIcon"
                      size="composerIcon"
                      className="queuedNextPromptButton"
                      aria-label="编辑排队输入"
                      onClick={handleEditQueuedNextPrompt}
                    >
                      <Pencil
                        className="composerLucideIcon"
                        aria-hidden="true"
                      />
                    </Button>
                  </Tooltip>
                  <Tooltip align="end" content="马上追加">
                    <Button
                      type="button"
                      variant="composerIcon"
                      size="composerIcon"
                      className="queuedNextPromptButton"
                      aria-label="马上追加排队输入"
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
              onSubmit={() => void handleSend()}
              onComposerAction={handleComposerAction}
              onModelSelect={(configured) => void selectGlobalModel(configured)}
              onReasoningEffortSelect={(effort) => void selectReasoningEffort(effort)}
              onCompact={() => void handleCompactContext()}
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
