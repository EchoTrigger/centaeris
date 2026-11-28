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

export interface ForkSessionRequest {
  operationId: string;
  sessionId: string;
  beforeTurnId: string;
}

export interface ForkSessionReceipt {
  operationId: string;
  sourceSessionId: string;
  sourceTurnId: string;
  sessionId: string;
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

/** Native typed-model data is JSON, with finite named criteria owned by Core. */
export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };

export interface TypedQuestion {
  type: "choice" | "noul";
  instructions: JsonValue;
  criteria: Record<string, string>;
}

export interface EvaluateTypedRequest {
  operationId: string;
  sessionId: string;
  /** Optional assertion; the dedicated CET process owns the selected model. */
  model?: string;
  state: JsonValue;
  questions: Record<string, TypedQuestion>;
}

export type TypedAnswer =
  | { type: "choice"; choice: string; confidence?: number; probabilities?: Record<string, number> }
  | { type: "noul"; noul: number };

export interface TypedModelRequest {
  model: string;
  state: JsonValue;
  questions: Record<string, TypedQuestion>;
}

export interface TypedModelResponse {
  model: string;
  answers: Record<string, TypedAnswer>;
  id?: string;
  usage?: JsonValue;
}

/** Projection of immutable CET request, raw response, and validated receipt files. */
export interface TypedModelReceipt {
  operationId: string;
  sessionId: string;
  attemptId: string;
  /** CET-generated identity sent to the provider as X-Request-Id. */
  requestId: string;
  upstreamRequestId: string | null;
  responseId: string | null;
  recordPath: string;
  request: TypedModelRequest;
  response: TypedModelResponse;
  /** Raw provider usage; no SDK token accounting or identity reconstruction. */
  usage: JsonValue;
}

export interface ListModelRequestsRequest {
  sessionId: string;
  agentRunId?: string;
  afterSequence?: number;
  limit?: number;
}

/** Canonical CET session-log wire event; Core owns the payload schema. */
export interface ModelRequestRecord {
  schemaVersion: "session.event.v1";
  eventVersion: 1;
  sequence: number;
  type: "model_request_started" | "provider_usage";
  eventId: string;
  sessionId: string;
  turnId?: string;
  agentRunId?: string;
  createdAtMs: number;
  payload: JsonValue;
}

export interface ModelRequestPage {
  sessionId: string;
  records: ModelRequestRecord[];
  nextSequence: number;
  hasMore: boolean;
}

export interface ReadTypedOperationRequest {
  operationId: string;
  sessionId: string;
}

/** Original HTTP evidence remains available even when finite answers fail validation. */
export interface TypedOperation {
  operationId: string;
  sessionId: string;
  attemptId: string;
  requestId: string;
  recordPath: string;
  request: TypedModelRequest;
  rawResponse: { statusCode: number; headers: Record<string, string>; body: string } | null;
  failure: JsonValue;
  receipt: TypedModelReceipt | null;
}

/** Native CET model configuration. Credentials are write-only. */
export type ModelWireApi = "openai-completions" | "openai-responses" | "anthropic-messages";
export interface RuntimeCustomModelDraft {
  model: string;
  displayName: string | null;
  contextTokens: string;
  maxOutputTokens: string;
  apiOverride: ModelWireApi | null;
  supportsVision: boolean;
}
export interface RuntimeCustomModelProviderDraft {
  providerId: string;
  name: string;
  baseUrl: string;
  api: ModelWireApi;
  models: RuntimeCustomModelDraft[];
}
export interface RuntimeSelectableModel {
  providerId: string;
  providerName: string;
  model: string;
  displayName: string | null;
  modelApiBase: string | null;
  modelContextTokens: number | null;
  modelMaxOutputTokens: number | null;
  modelThinkingMode: string | null;
  modelThinkingModes: string[];
  preferredThinkingMode: string | null;
  modelApi: ModelWireApi | null;
}
export interface RuntimeCatalogModel extends RuntimeSelectableModel {
  supportsVision: boolean;
  builtIn: boolean;
  diagnostic: string | null;
}
export interface RuntimeModelProvider {
  providerId: string;
  name: string;
  tier: string | null;
  logoSvg: string | null;
  builtIn: boolean;
  accessKind: string;
  configured: boolean;
  credentialSource: "stored" | "environment" | null;
  models: RuntimeCatalogModel[];
}
export interface RuntimeConfig {
  executionHost: string;
  bashPath: string | null;
  autoContinueAfterResumeWait: boolean;
  agentTransportMode: string;
  modelProviderId: string | null;
  model: string | null;
  modelProviders: RuntimeModelProvider[];
  selectableModels: RuntimeSelectableModel[];
  customModelProviders: RuntimeCustomModelProviderDraft[];
  modelApiBase: string | null;
  modelTimeoutMs: number | null;
  modelMaxRetries: number | null;
  modelRetryBackoffMs: number | null;
  modelContextTokens: number | null;
  modelMaxOutputTokens: number | null;
  modelThinkingMode: string | null;
  toolParallelism: number | null;
  updatedAt: number;
}
export interface SetRuntimeConfigRequest {
  bashPath?: string;
  autoContinueAfterResumeWait?: boolean;
  agentTransportMode?: string;
  modelProviderId?: string;
  model?: string;
  modelThinkingMode?: string;
  /** Sent only to CET; never included in RuntimeConfig responses. */
  modelApiKey?: string;
  clearModelApiKey?: boolean;
  /** Complete replacement collection, validated and persisted by CET. */
  customModelProviders?: RuntimeCustomModelProviderDraft[];
  toolParallelism?: number;
}

/** Native per-profile plugin activation; model/session semantics remain in CET. */
export interface SetPluginEnabledRequest {
  id: string;
  enabled: boolean;
}
export interface PluginCatalogState {
  schema: "plugin_catalog_state_v1";
  enabledPlugins: string[];
  disabledPlugins: string[];
}
