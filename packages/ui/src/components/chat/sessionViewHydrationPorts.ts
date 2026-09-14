import type { Dispatch, RefObject, SetStateAction } from "react";
import type {
  AgentContextUsageSummary,
  AgentRuntimeConfig,
  AgentStreamPayload,
} from "../../lib/chatBridge";
import type {
  SessionReplayCursors,
} from "../../lib/sessionViewCache";
import type { UiSession } from "../../types/ui";
import type { AssistantTurnUpdateQueue } from "./useAssistantTurnUpdateQueue";
import type { DesktopTranscriptView } from "./transcriptPaging";
import type {
  ActiveStreamState,
  CachedActiveReplay,
  ChatMessage,
  PendingQuestionState,
} from "./types";

type SetMessages = (
  updater: ChatMessage[] | ((previous: ChatMessage[]) => ChatMessage[]),
) => void;

export type HydrationViewPort = {
  currentSessionId: string;
  messagesRef: RefObject<ChatMessage[]>;
  visibleSessionIdRef: RefObject<string>;
  transcriptViewRef: RefObject<DesktopTranscriptView | null>;
  transcriptHistoryMessageCountRef: RefObject<number>;
  setMessages: SetMessages;
  setTranscriptHasOlder: Dispatch<SetStateAction<boolean>>;
  setSessionLoadError: Dispatch<SetStateAction<string>>;
  setEditingUserMessageId: Dispatch<SetStateAction<string | null>>;
  setEditingPrompt: Dispatch<SetStateAction<string>>;
};

export type HydrationReplayPort = {
  replayCursorsByAgentRunIdRef: RefObject<SessionReplayCursors>;
  verifiedReplayAgentRunIdsRef: RefObject<Set<string>>;
  visibleActiveReplayRef: RefObject<CachedActiveReplay | null>;
  persistVisibleSessionViewCache: (sessionId?: string) => void;
};

export type HydrationRuntimePort = {
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

export type HydrationQuestionPort = {
  setPendingQuestion: Dispatch<SetStateAction<PendingQuestionState | null>>;
  setPendingQuestionError: Dispatch<SetStateAction<string>>;
};

export type HydrationStreamPort = {
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

export type HydrationSessionOutcomePort = {
  pendingResolvedSessionRef: RefObject<UiSession | null>;
  preserveResolvedSessionIdRef: RefObject<string | null>;
};

export type SessionViewHydrationControllerOptions = {
  view: HydrationViewPort;
  replay: HydrationReplayPort;
  runtime: HydrationRuntimePort;
  question: HydrationQuestionPort;
  stream: HydrationStreamPort;
  sessionOutcome: HydrationSessionOutcomePort;
};
