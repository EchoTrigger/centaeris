import type { UiSession } from "../../types/ui";
import type {
  AgentContextUsageSummary,
  AgentRuntimeConfig,
  AgentStreamPayload,
} from "../../lib/chatBridge";
import type { SessionReplayCursors } from "../../lib/sessionViewCache";
import type {
  WorkspaceGitHubCliStatusResponse,
  WorkspaceGitStatusResponse,
} from "../../lib/workspaceBridge";

export type ChatAreaProps = {
  currentSession: UiSession | null;
  currentSessionId?: string | null;
  workspaceName: string;
  workspaceRoot?: string | null;
  gitStatus?: WorkspaceGitStatusResponse | null;
  gitStatusError?: string;
  githubCliStatus?: WorkspaceGitHubCliStatusResponse | null;
  runtimeConfigRevision?: number;
  isPinnedSummaryOpen?: boolean;
  isPinnedSummaryRetracting?: boolean;
  onOpenWorkspacePath?: (
    path: string,
    options?: { startLine?: number; endLine?: number; taskId?: string },
  ) => void;
  onOpenAgentSession?: (sessionId: string, title: string) => void;
  onNewSession?: () => void;
  onOpenResource?: (resource: "models" | "skills" | "plugins") => void;
  onSessionResolved?: (
    session: UiSession,
    options?: { activate?: boolean },
  ) => void;
  onAgentRunningChange?: (sessionId: string, isRunning: boolean) => void;
  onSessionCompleted?: (sessionId: string) => void;
};

export type ChatViewMode = "conversation" | "restoring" | "welcome";

export type ChatMessage =
  | {
    id: string;
    role: "user";
    text: string;
    timestamp?: number;
  }
  | {
    id: string;
    role: "assistant";
    turn: AssistantExecutionTurn;
    status?: string;
  };

export type TaskStatus = "running" | "done" | "error";

export type RuntimeActivityKind =
  | "thinking"
  | "planning"
  | "searching"
  | "reading"
  | "executing"
  | "reviewing"
  | "compressing"
  | "recovering"
  | "retrying"
  | "waiting"
  | "outputting"
  | "summarizing";

export type RuntimeProcessState =
  | "thinking"
  | "searching"
  | "reading"
  | "executing"
  | "reviewing"
  | "synthesizing"
  | "compressing"
  | "recovering"
  | "retrying"
  | "waiting"
  | "provider_waiting"
  | "auth_failed"
  | "provider_unavailable"
  | "provider_interrupted"
  | "unknown";

export type RuntimeActivity = {
  kind: RuntimeActivityKind;
  label: string;
  processState?: RuntimeProcessState;
};

export type ToolOperation = {
  callId: string;
  toolName: string;
  kind?: "command";
  status: string;
  resultState?: string;
  path?: string;
  startLine?: number;
  endLine?: number;
  totalLines?: number;
  nextOffset?: number;
  truncatedBy?: string;
  query?: string;
  matchCount?: number;
  added?: number;
  removed?: number;
  lines?: number;
  text?: string;
  outputPreview?: string;
  diffPreview?: string;
  error?: string;
  exitCode?: number;
};

export type TaskResult = {
  id: string;
  turnId?: string;
  title: string;
  summary: string;
  status: TaskStatus;
  provider: "workspace" | "ui" | "tool";
  durationMs?: number;
  operations?: ToolOperation[];
  normalizedInput?: Record<string, unknown>;
  displayTarget?: string;
  modelContent?: string;
  fullOutputPath?: string;
  outputStartByte?: number;
  outputByteLength?: number;
  waterfall?: EventWaterfall;
};

export type SubagentToolGroup = {
  id: string;
  title: string;
  summary: string;
  status: TaskStatus;
  stats?: Record<string, unknown>;
  details?: unknown;
  sourceEventIds?: string[];
};

export type SubagentResult = {
  id: string;
  subagentId: string;
  turnId?: string;
  taskId?: string;
  parentTaskId?: string | null;
  childSessionId?: string;
  displayName?: string;
  avatarSeed?: string;
  role?: string;
  title: string;
  summary: string;
  description?: string;
  status: TaskStatus;
  resultPreview?: string;
  startedAtMs?: number;
  completedAtMs?: number;
  workPacketSummary?: Record<string, unknown>;
  resultEnvelope?: Record<string, unknown>;
  payload?: Record<string, unknown>;
  toolGroups?: SubagentToolGroup[];
  waterfall?: EventWaterfall;
};

export type NarrativeChunk = {
  id: string;
  kind: "narrative";
  turnId?: string;
  text: string;
  phase?: string;
  ephemeral?: boolean;
  scope?: string;
  streamKey?: string;
  sourceItemId?: string;
  tone?: "normal" | "error";
  waterfall?: EventWaterfall;
};

export type GuidedSupplementChunk = {
  id: string;
  kind: "guidedSupplement";
  text: string;
  timestamp: number;
  waterfall?: EventWaterfall;
};

export type TaskChunk = {
  id: string;
  kind: "task";
  task: TaskResult;
};

export type SubagentChunk = {
  id: string;
  kind: "subagent";
  subagent: SubagentResult;
};

export type TranscriptWaterfallSection = "process" | "tool" | "subagent" | "final";

export type EventWaterfall = {
  schema?: string;
  section?: TranscriptWaterfallSection | string;
  groupId?: string;
  displayRole?: string;
  collapsePolicy?: string;
  order?: number;
  [key: string]: unknown;
};

export type AssistantExecutionTurn = {
  id: string;
  agentRunId?: string;
  chunks: Array<
    NarrativeChunk | GuidedSupplementChunk | TaskChunk | SubagentChunk
  >;
  finalAnswer: string;
  isStreaming: boolean;
  startedAtMs?: number;
  completedAtMs?: number;
  activity?: RuntimeActivity | null;
};

export type NarrativeProjectionMeta = {
  phase?: string;
  ephemeral?: boolean;
  scope?: string;
  streamKey?: string;
  sourceItemId?: string;
};

export type ActiveStreamState = {
  sessionId: string;
  agentRunId: string;
  assistantMessageId: string;
  seenSessionEvent: boolean;
  seenSessionEventIds: Set<string>;
  close: () => void;
};

export type StreamSeenSets = {
  seenSessionEventIds: Set<string>;
  seenSessionEvent: boolean;
};

export type PendingQuestionRequest = {
  id: string;
  question: string;
  options: string[];
  multiSelect: boolean;
  required: boolean;
};

export type PendingQuestionState = {
  assistantMessageId: string;
  request: PendingQuestionRequest;
  selectedOptions: string[];
  answerText: string;
  submitting: boolean;
};

export type ModelRuntimeDraft = {
  modelProviderId: string;
  model: string;
  modelApiBase: string;
  modelTimeoutMs: string;
  modelMaxRetries: string;
  modelRetryBackoffMs: string;
  modelContextTokens: string;
  modelMaxOutputTokens: string;
  modelThinkingMode: string;
};

export type CachedActiveReplay = {
  messageId: string;
  agentRunId: string;
};

export type SessionViewSnapshot = {
  messages: ChatMessage[];
  contextUsage: AgentContextUsageSummary | null;
  autoContinueAfterResumeWait: boolean | undefined;
  pendingQuestion: PendingQuestionState | null;
  pendingQuestionError: string;
  activeReplay: CachedActiveReplay | null;
};

export type AgentResultStreamProps = {
  turn: AssistantExecutionTurn;
  onOpenAgentSession?: (sessionId: string, title: string) => void;
  onOpenWorkspacePath?: (
    path: string,
    options?: { startLine?: number; endLine?: number; taskId?: string },
  ) => void;
};

export type SessionHydrationSnapshot = {
  messages: ChatMessage[];
  runtimeConfig: AgentRuntimeConfig;
  contextUsage: AgentContextUsageSummary | null;
  resolvedAutoContinueAfterResumeWait: boolean | undefined;
  replayCursorsByAgentRunId: SessionReplayCursors;
  pendingQuestionRequest: PendingQuestionRequest | null;
  restoreMessageId: string | null;
  activeReplay: {
    messageId: string;
    agentRunId: string;
    status: string;
    seedPayloads: AgentStreamPayload[];
  } | null;
};

export type AgentRunReplaySnapshot = {
  items: AgentStreamPayload[];
  nextCursor: number;
};

export type AgentDisplayEntry =
  | {
    kind: "narrative";
    chunk: NarrativeChunk;
  }
  | {
    kind: "guidedSupplement";
    chunk: GuidedSupplementChunk;
  }
  | {
    kind: "taskGroup";
    id: string;
    tasks: TaskResult[];
  }
  | {
    kind: "subagent";
    chunk: SubagentChunk;
  };

export type TranscriptTextPhase = "process" | "compaction" | "streaming" | "final";

export type TranscriptTextItem = {
  kind: "assistantText";
  id: string;
  phase: TranscriptTextPhase;
  text: string;
  tone?: "normal" | "error";
  processState?: string;
  compactLabel?: string;
  detail?: string;
  severity?: string;
  source?: string;
  turnId?: string;
  waterfall?: EventWaterfall;
};

export type TranscriptToolGroupItem = {
  kind: "toolGroup";
  id: string;
  turnId?: string;
  tasks: TaskResult[];
  waterfall?: EventWaterfall;
};

export type TranscriptToolLikeItem = TranscriptToolGroupItem;

export type TranscriptItem =
  | TranscriptTextItem
  | {
    kind: "guidedSupplement";
    id: string;
    text: string;
    timestamp: number;
    waterfall?: EventWaterfall;
  }
  | TranscriptToolGroupItem;

export type TranscriptProcessSection = {
  id: string;
  heading?: TranscriptTextItem;
  items: TranscriptItem[];
};

export type TranscriptViewModel = {
  processItems: TranscriptItem[];
  processSections: TranscriptProcessSection[];
  finalItem: TranscriptTextItem | null;
};

export type TimelineOperation = ToolOperation & {
  taskId: string;
  taskTitle: string;
  durationMs?: number;
  normalizedInput?: Record<string, unknown>;
  displayTarget?: string;
  modelContent?: string;
  fullOutputPath?: string;
  outputStartByte?: number;
  outputByteLength?: number;
};
