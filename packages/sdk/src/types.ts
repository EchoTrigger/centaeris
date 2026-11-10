/** One JSON-RPC message per frame, with transport-owned connection lifetime. */
export interface RpcTransport {
  send(frame: string): void | Promise<void>;
  listen(receiver: RpcReceiver): () => void;
  close(): void;
}

export interface RpcReceiver {
  message(frame: string): void;
  close(cause?: Error): void;
}

export interface ClientRegistration {
  viewerId: string;
  clientKind?: "sdk" | "desktop" | "tui";
}

export interface RuntimeDescriptor {
  status: "ok";
  runtime: string;
  protocol: "centaeris.runtime";
  protocolVersion: 1;
  capabilities: string[];
  events: string[];
  projections: string[];
  buildId: string;
  coreProtocolVersion: string;
  profileId: string;
  storeId: string;
  storeSchemaVersion: number;
  layoutSchemaVersion: number;
}

export interface CreateSessionRequest {
  operationId: string;
  cwd: string;
  title?: string | null;
}

export interface LoadSessionRequest {
  sessionId: string;
}

export interface SessionItem {
  id: string;
  title: string;
  updatedAt: number;
  lastMessage: string | null;
  cwd: string | null;
  sessionKind: string;
  parentSessionId: string | null;
  runtimeJobId: string | null;
  sortOrder: number;
  isPinned: boolean;
  isUnread: boolean;
  messageCount: number;
  activityState: string;
}

export interface PersistedMessage {
  id: string;
  sessionId: string;
  turnId: string;
  role: string;
  content: string;
  status?: string;
  createdAtMs: number;
  updatedAtMs: number;
  agentRunId?: string;
  imageData?: unknown;
}

export interface SessionData {
  id: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  sessionKind: string;
  parentSessionId: string | null;
  runtimeJobId: string | null;
  messages: PersistedMessage[];
}

export interface SendRequest {
  operationId: string;
  sessionId?: string | null;
  message: string;
  tailPolicy?: "append" | "rewriteLastUser" | null;
  rewriteTargetMessageId?: string | null;
  rewriteExpectedTailMessageId?: string | null;
  autoContinueAfterResumeWait?: boolean | null;
  attachments?: { placeholder: string; localPath: string }[];
}

export type SessionPrompt = Omit<SendRequest, "sessionId">;

export interface SupplementRequest {
  sessionId: string;
  agentRunId: string;
  message: string;
}

export interface SupplementReceipt {
  accepted: boolean;
  sessionId: string;
  agentRunId: string;
  queuedCount: number;
}

export interface AnswerNowRequest {
  schema: "agent_run.intervention.v1";
  interventionId: string;
  agentRunId: string;
  kind: "answer_now";
}

export interface AnswerNowReceipt {
  accepted: boolean;
  disposition: "accepted" | "alreadyConverging";
  sessionId: string;
  agentRunId: string;
  interventionId: string;
}

export interface QuestionAnswerRequest {
  sessionId?: string | null;
  questionId: string;
  answers?: string[] | null;
  answerText?: string | null;
  autoContinueAfterResumeWait?: boolean | null;
}

export type SessionQuestionAnswer = Omit<QuestionAnswerRequest, "sessionId">;

export interface CompactContextRequest {
  sessionId: string;
}

export interface CompactContextReceipt {
  sessionId: string;
  compacted: boolean;
}

export interface ListRunsRequest {
  sessionId?: string | null;
  includeTerminal?: boolean | null;
}

export interface RunList {
  agentRuns: AgentRunSummary[];
}

/** The durable admission receipt returned before the Run finishes. */
export interface SendReceipt {
  sessionId: string;
  agentRunId: string;
  turnId: string;
  streamItems: unknown[];
}

export interface RunTarget {
  sessionId?: string | null;
  agentRunId?: string | null;
}

export interface AgentRunSummary {
  agentRunId: string;
  sessionId: string;
  turnId: string;
  agentRunKind: string;
  cwd: string | null;
  status: string;
  unread: boolean;
  startedAtMs: number;
  updatedAtMs: number;
  completedAtMs: number | null;
  lastEventAtMs: number | null;
  stallReason: string | null;
  watchdog: unknown;
  error: string | null;
}

export interface AgentRun {
  agentRunId: string;
  sessionId: string;
  turnId: string;
  agentRunKind: string;
  cwd: string | null;
  status: string;
  startedAtMs: number;
  updatedAtMs: number;
  completedAtMs: number | null;
  lastEventAtMs: number | null;
  stallReason: string | null;
  watchdog: unknown;
  error: string | null;
}

export interface RunSubscription {
  agentRun: AgentRun | null;
  viewerId: string;
  transitionReason: string;
  attachedViewerCount: number;
}

export interface SessionCancelReceipt {
  agentRun: AgentRun | null;
  cancelAccepted: boolean;
}

export interface SubscriptionReceipt {
  agentRun: AgentRunSummary | null;
  viewerId: string;
  transitionReason: string;
  attachedViewerCount: number;
}

export interface CancelRequest extends RunTarget {
  reason?: string | null;
}

/** Cancellation admission and the observed Run summary at that moment. */
export interface CancelReceipt {
  agentRun: AgentRunSummary | null;
  cancelAccepted: boolean;
}

/** Opaque canonical notification content, with identities available on its envelope. */
export interface SessionUpdate {
  params: unknown;
  sessionId: string | undefined;
  agentRunId: string | undefined;
  turnId: string | undefined;
}
