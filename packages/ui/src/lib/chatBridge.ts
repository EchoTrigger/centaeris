import { invokeHost, isNativeHostRuntime, listenHost } from "../host/hostBridge";
import {
  appendCoalescedStreamPayload,
  compactConsumedStreamPayloads,
} from "./streamPayloadCoalescing";

export type SendAgentInputRequest = {
  operationId: string;
  sessionId?: string;
  message: string;
  imageData?: string | string[];
  agentName?: string;
  personalityPrompt?: string;
  userSalutation?: string;
  preferredLocale?: string;
  model?: string;
  enableThinking?: boolean;
  autoContinueAfterResumeWait?: boolean;
  tailPolicy?: "append" | "rewriteLastUser";
  rewriteTargetMessageId?: string;
  rewriteExpectedTailMessageId?: string;
};

type AgentTransportMode = "desktop_primary";

type JsonObject = Record<string, unknown>;

type SendAgentInputResponse = {
  sessionId?: string;
  agentRunId?: string;
  turnId?: string;
};

export const createRuntimeOperationId = (): string => crypto.randomUUID();

type SendAgentSupplementRequest = {
  sessionId: string;
  agentRunId: string;
  message: string;
  imageData?: string | string[];
};

type SendAgentSupplementResponse = {
  accepted?: boolean;
  sessionId?: string;
  agentRunId?: string;
  queuedCount?: number;
};

type AnswerAgentQuestionRequest = {
  sessionId?: string;
  questionId: string;
  answers?: string[];
  answerText?: string;
  autoContinueAfterResumeWait?: boolean;
};

type AgentInitResponse = {
  sessionId?: string;
  agentRunId?: string;
  turnId?: string;
};

export type SessionItem = {
  id: string;
  title: string;
  updatedAt: number;
  lastMessage?: string;
  cwd?: string;
  sessionKind: "main" | "subagent";
  parentSessionId?: string;
  runtimeJobId?: string;
  sortOrder?: number;
  isPinned?: boolean;
  isUnread?: boolean;
  messageCount: number;
  activityState: "idle" | "inactive";
};

export type SessionReorderSection = "pinned" | "recent";

export type SessionDeleteResponse = {
  deletedSessionId: string;
};

export type PersistedChatMessage = {
  id?: string;
  sessionId?: string;
  turnId?: string;
  role?: string;
  content?: string;
  status?: "running" | "done" | "error" | string;
  createdAtMs?: number;
  updatedAtMs?: number;
  agentRunId?: string;
  imageData?: string | string[];
};

export type SessionData = {
  id?: string;
  title?: string;
  createdAt?: number;
  updatedAt?: number;
  sessionKind: "main" | "subagent";
  parentSessionId?: string;
  runtimeJobId?: string;
  messages?: PersistedChatMessage[];
};

export type SessionAgentRunReplayProjection = {
  agentRunId: string;
  sessionId: string;
  turnId: string;
  status: AgentRunStatus;
  startedAtMs: number;
  updatedAtMs: number;
  completedAtMs?: number | null;
  nextCursor: number;
  items: AgentStreamPayload[];
};

export type SessionProjectionData = {
  schemaVersion: "session_projection.v1";
  session: SessionData;
  agentRuns: AgentRunSummary[];
  agentRunReplays: SessionAgentRunReplayProjection[];
  activeAgentRunId?: string | null;
};

export type PendingQuestionSummary = {
  question_id: string;
  created_at: number;
  turn_id: string;
  question_request: Record<string, unknown>;
};

export type AgentCheckpointSummary = {
  turn_id?: string;
  status?: string;
  done_reason?: string;
  updated_at?: number;
  error?: string;
  message_count?: number;
  loop_count?: number;
  web_search_count?: number;
  state?: Record<string, unknown>;
};

export type AgentStateSummary = {
  session_id: string;
  pending_question_count: number;
  pending_questions: PendingQuestionSummary[];
  checkpoint?: AgentCheckpointSummary | null;
};

export type AgentRunStatus =
  | "queued"
  | "running"
  | "waiting_user"
  | "completed"
  | "failed"
  | "cancelled"
  | "stopped"
  | "stalled"
  | string;

export type AgentRunSummary = {
  agentRunId: string;
  sessionId: string;
  turnId: string;
  agentRunKind?: string;
  cwd?: string | null;
  status: AgentRunStatus;
  unread: boolean;
  startedAtMs: number;
  updatedAtMs: number;
  completedAtMs?: number | null;
  lastEventAtMs?: number | null;
  stallReason?: string | null;
  watchdog?: {
    noTranscriptForMs?: number | null;
    repeatedToolSignatureCount: number;
    repeatedProcessTextCount: number;
    lastProgressReason?: string | null;
  } | null;
  error?: string | null;
};

export type AgentRunListRequest = {
  sessionId?: string;
  includeTerminal?: boolean;
};

export type AgentRunListResponse = {
  agentRuns: AgentRunSummary[];
};

export type AgentRunStreamReplayRequest = {
  agentRunId: string;
  cursor?: number;
  limit?: number;
};

export type AgentRunStreamReplayResponse = {
  agentRunId: string;
  cwd?: string | null;
  items: AgentStreamPayload[];
  nextCursor?: number | null;
};

export type AgentRunAttachRequest = {
  agentRunId?: string;
  sessionId?: string;
  viewerId?: string;
};

export type AgentRunDetachRequest = {
  agentRunId?: string;
  sessionId?: string;
  viewerId?: string;
};

export type AgentRunCancelRequest = {
  agentRunId?: string;
  sessionId?: string;
  reason?: string;
};

export type AgentRunAttachResponse = {
  agentRun?: AgentRunSummary | null;
  viewerId?: string;
  transitionReason?: string;
  attachedViewerCount?: number;
};

export type AgentRunDetachResponse = {
  agentRun?: AgentRunSummary | null;
  viewerId?: string;
  transitionReason?: string;
  attachedViewerCount?: number;
};

export type AgentRunCancelResponse = {
  agentRun?: AgentRunSummary | null;
  cancelled: boolean;
};

export type PluginDescriptorV1 = {
  id: string;
  name: string;
  description: string;
  source: string;
  enabled: boolean;
  path: string;
  manifestPath?: string | null;
  errors: string[];
  version?: string | null;
  tools: string[];
  scopes: string[];
  activationStatus: string;
  policySource: string;
};

export type PluginCatalogStateV1 = {
  schema: string;
  enabledPlugins: string[];
  disabledPlugins: string[];
};

export type PluginCapabilitiesV1 = {
  skills: string[];
  cli: string[];
  mcpServers: string[];
  apps: string[];
  hooks: string[];
  capabilities: string[];
};

export type PluginDetailV1 = {
  descriptor: PluginDescriptorV1;
  capabilities: PluginCapabilitiesV1;
};

export type PluginSourceRefV1 = {
  kind: string;
  path: string;
};

export type PluginRevealSourceRefResponseV1 = PluginSourceRefV1 & {
  opened: boolean;
};

export type PluginInstallSelectionResponseV1 = {
  cancelled: boolean;
  plugin: PluginDetailV1 | null;
};

export type PluginRemoveResponseV1 = {
  removedId: string;
  catalog: PluginCatalogStateV1;
};

export type NativeMcpServerStatus =
  | "ready"
  | "needsConfiguration"
  | "disabled"
  | "unsupported";

export type NativeMcpServer = {
  pluginName: string;
  pluginDisplayName: string;
  serverId: string;
  pluginEnabled: boolean;
  status: NativeMcpServerStatus;
  configurable: boolean;
  configured: boolean;
  transport: "stdio" | "streamableHttp";
  endpoint?: string | null;
  toolNames: string[];
};

export type NativeMcpCatalog = {
  schema: "native.mcp.catalog.v1";
  servers: NativeMcpServer[];
};

export type SkillSourceScope = "workspace" | "user" | "system" | "plugin";
export type SkillSourceKind = "catalogDirectory" | "skillFile";

export type SkillSourceConfig = {
  sourceId: string;
  scope: SkillSourceScope;
  kind: SkillSourceKind;
  path: string;
  workspaceRoot?: string | null;
  enabled: boolean;
};

export type SkillPolicy = {
  sourceId: string;
  skillName: string;
  enabled: boolean;
};

export type SkillSourcesConfig = {
  schemaVersion: "skill.sources.v1";
  sources: SkillSourceConfig[];
  skillPolicies: SkillPolicy[];
};

export type SkillCapabilityMetadata = {
  allowedTools: string[];
};

export type SkillEntry = {
  skillId: string;
  sourceId: string;
  scope: SkillSourceScope;
  name: string;
  description: string;
  enabled: boolean;
  allowImplicitInvocation: boolean;
  capabilityMetadata: SkillCapabilityMetadata;
  skillMdPath: string;
  rootPath: string;
  contentHash: string;
  shadowedBy?: string | null;
  errors: string[];
};

export type SkillSourceStatus = SkillSourceConfig;

export type SkillDiagnostic = {
  code: string;
  message: string;
  sourceId?: string | null;
  path?: string | null;
};

export type SkillCatalogSnapshot = {
  schema: "skill_catalog_snapshot_v1";
  cwd?: string | null;
  catalogHash: string;
  sources: SkillSourceStatus[];
  skills: SkillEntry[];
  diagnostics: SkillDiagnostic[];
};

export type SkillDetail = {
  skill: SkillEntry;
  content: string;
};

export type SkillSourcePathSelection = {
  cancelled: boolean;
  path?: string | null;
};

export type SkillSourceRevealResponse = {
  kind: "local_path";
  path: string;
  opened: boolean;
};

export type AgentContextUsageSummary = {
  sessionId: string;
  usedTokens?: number | null;
  maxContextTokens?: number | null;
  usedPercentage?: number | null;
  updatedAt?: number | null;
  isCompacting: boolean;
  latestUsage?: AgentTokenUsageSummary | null;
  breakdown?: AgentContextTokenBreakdown | null;
};

export type AgentContextTokenBreakdown = {
  systemPromptTokens: number;
  systemToolTokens: number;
  mcpToolTokens: number;
  skillsTokens: number;
  messageTokens: number;
  autoCompactBufferTokens: number;
  freeSpaceTokens: number;
  mcpTools: Array<{ providerId: string; name: string; tokens: number }>;
};

export type AgentContextCompactResponse = {
  sessionId: string;
  compacted: boolean;
};

export type AgentTokenUsageSummary = {
  inputTokens?: number | null;
  outputTokens?: number | null;
  totalTokens?: number | null;
  promptCacheHitTokens?: number | null;
  promptCacheMissTokens?: number | null;
  promptCacheTotalTokens?: number | null;
  promptCacheHitRate?: number | null;
};

export type AgentRuntimeConfig = {
  executionHost: "localUser";
  bashPath?: string | null;
  autoContinueAfterResumeWait: boolean;
  agentTransportMode?: AgentTransportMode;
  modelProviderId?: string;
  model?: string;
  modelProviders: ModelProvider[];
  selectableModels: SelectableModel[];
  customModelProviders?: CustomModelProvider[];
  modelApiBase?: string;
  modelTimeoutMs?: number;
  modelMaxRetries?: number;
  modelRetryBackoffMs?: number;
  modelContextTokens?: number;
  modelMaxOutputTokens?: number;
  modelThinkingMode?: string;
  toolParallelism?: number;
  updatedAt: number;
};

export type AgentRuntimeConfigResetResponse = {
  config: AgentRuntimeConfig;
  quarantinedPath?: string | null;
};

export type SelectableModel = {
  providerId: string;
  providerName: string;
  model: string;
  displayName?: string | null;
  modelApiBase?: string | null;
  modelContextTokens?: number | null;
  modelMaxOutputTokens?: number | null;
  modelThinkingMode?: string | null;
  modelThinkingModes: ModelThinkingMode[];
  modelApi?: ModelWireApi | null;
};

export type ModelCatalogItem = {
  providerId: string;
  providerName: string;
  model: string;
  displayName?: string | null;
  modelApiBase?: string | null;
  modelContextTokens?: number | null;
  modelMaxOutputTokens?: number | null;
  modelThinkingMode?: string | null;
  modelThinkingModes: ModelThinkingMode[];
  supportsVision: boolean;
  builtIn: boolean;
  modelApi?: ModelWireApi | null;
  diagnostic?: string | null;
};

export type ModelProvider = {
  providerId: string;
  name: string;
  builtIn: boolean;
  accessKind: "api_key" | "custom";
  configured: boolean;
  credentialSource?: "stored" | "environment" | null;
  models: ModelCatalogItem[];
};

export type ModelWireApi =
  | "openai-completions"
  | "openai-responses"
  | "anthropic-messages";

export type ModelThinkingMode =
  | "none"
  | "low"
  | "medium"
  | "high"
  | "xhigh"
  | "max";

export type CustomModelProvider = {
  providerId: string;
  name: string;
  baseUrl: string;
  api: ModelWireApi;
  models: CustomModelProviderModelInput[];
};

export type CustomModelProviderInput = CustomModelProvider & {
  models: CustomModelProviderModelInput[];
};

export type CustomModelProviderModelInput = {
  model: string;
  displayName?: string;
  contextTokens: string;
  maxOutputTokens: string;
  apiOverride?: ModelWireApi;
  supportsVision: boolean;
};

export type RuntimeGarbageCollectRequest = {
  dryRun?: boolean;
  documentCacheGraceMs?: number;
};

export type RuntimeGarbageCollectItem = {
  kind: string;
  path: string;
  sizeBytes: number;
  modifiedAtMs?: number | null;
  expiresAtMs: number;
  reason: string;
  deleted: boolean;
};

export type RuntimeGarbageCollectResponse = {
  schema: string;
  dryRun: boolean;
  dataRoot: string;
  candidateCount: number;
  deletedCount: number;
  totalCandidateBytes: number;
  totalDeletedBytes: number;
  items: RuntimeGarbageCollectItem[];
  generatedAtMs: number;
};

export type RuntimeJobStatus =
  | "queued"
  | "leased"
  | "running"
  | "succeeded"
  | "failed"
  | "dead_lettered"
  | "cancelled"
  | string;

export type RuntimeJobRecord = {
  jobId: string;
  jobKind: string;
  status: RuntimeJobStatus;
  runAtMs: number;
  leaseOwner?: string | null;
  leaseExpiresAtMs?: number | null;
  retryCount: number;
  maxRetries: number;
  backoffPolicy?: JsonObject;
  idempotencyKey: string;
  sessionId?: string | null;
  branchId?: string | null;
  checkpointId?: string | null;
  payloadRef?: string | null;
  outputRefs?: string[];
  lastError?: string | null;
  createdAtMs: number;
  updatedAtMs: number;
};

export type ScheduleRuntimeJobDisposition = "inserted" | "existing" | string;

export type ScheduleRuntimeJobResult = {
  disposition: ScheduleRuntimeJobDisposition;
  job: RuntimeJobRecord;
};

export type AgentRuntimeJobListRequest = {
  statuses?: RuntimeJobStatus[];
  jobKind?: string;
  sessionId?: string;
  branchId?: string;
  providerId?: string;
  providerToolName?: string;
  limit?: number;
  offset?: number;
};

export type AgentRuntimeJobListResponse = {
  jobs: RuntimeJobRecord[];
};

export type AgentRuntimeJobGetResponse = {
  job?: RuntimeJobRecord | null;
};

export type DeadLetterStatus =
  | "open"
  | "replaying"
  | "replayed"
  | "dismissed"
  | string;

export type DeadLetterRecord = {
  deadLetterId: string;
  originalJobId: string;
  jobKind: string;
  status: DeadLetterStatus;
  sessionId?: string | null;
  branchId?: string | null;
  checkpointId?: string | null;
  payloadRef?: string | null;
  idempotencyKey: string;
  failureReason: string;
  lastError: string;
  attempts: number;
  firstFailedAtMs: number;
  lastFailedAtMs: number;
  replayPolicy?: JsonObject;
  replayedJobId?: string | null;
  dismissedBy?: string | null;
  dismissedReason?: string | null;
  updatedAtMs: number;
};

export type AgentDeadLetterListRequest = {
  statuses?: DeadLetterStatus[];
  jobKind?: string;
  sessionId?: string;
  branchId?: string;
  providerId?: string;
  providerToolName?: string;
  limit?: number;
  offset?: number;
};

export type AgentDeadLetterListResponse = {
  deadLetters: DeadLetterRecord[];
};

export type AgentDeadLetterGetResponse = {
  deadLetter?: DeadLetterRecord | null;
};

export type AgentDeadLetterReplayRequest = {
  deadLetterId: string;
  replayKey?: string;
  jobId?: string;
  idempotencyKey?: string;
  runAtMs?: number;
  maxRetries?: number;
};

export type AgentDeadLetterDismissRequest = {
  deadLetterId: string;
  dismissedBy?: string;
  dismissedReason?: string;
};

export type AgentDeadLetterActionResponse = {
  ok?: boolean;
  [key: string]: unknown;
};

type SetAgentRuntimeConfigRequest = {
  bashPath?: string;
  autoContinueAfterResumeWait?: boolean;
  agentTransportMode?: AgentTransportMode;
  modelProviderId?: string;
  model?: string;
  modelThinkingMode?: ModelThinkingMode | "default";
  modelApiKey?: string;
  clearModelApiKey?: boolean;
  customModelProviders?: CustomModelProviderInput[];
  toolParallelism?: number;
};

export type StreamSegment = {
  kind?: string;
  text?: string;
  at?: number;
  eventId?: string;
  stage?: string;
  taskId?: string;
  parentTaskId?: string;
  status?: string;
  uiKind?: string;
  meta?: Record<string, unknown>;
};

export type SessionEvent = {
  id?: string;
  version?: string;
  type?: string;
  at?: number;
  sessionId?: string;
  turnId?: string;
  agentRunId?: string;
  taskId?: string;
  parentTaskId?: string | null;
  status?: string;
  visibility?: string;
  toolName?: string;
  processState?: string;
  payload?: Record<string, unknown>;
  meta?: Record<string, unknown>;
};

export type TranscriptStreamItem = {
  id?: string;
  version?: string;
  sourceEventId?: string;
  turnId?: string;
  kind?: string;
  slot?: "final_answer" | string;
  streamKey?: string;
  phase?: string;
  text?: string;
  append?: boolean;
  replace?: boolean;
  ephemeral?: boolean;
  scope?: "turn" | "step" | string;
  title?: string;
  summary?: string;
  description?: string;
  status?: string;
  processState?: string;
  compactLabel?: string;
  detail?: string | null;
  severity?: "normal" | "attention" | "error" | string;
  source?: string;
  eventType?: string;
  toolName?: string;
  toolGroupId?: string | null;
  subagentId?: string;
  startedAtMs?: number;
  completedAtMs?: number | null;
  role?: string;
  taskId?: string;
  parentTaskId?: string | null;
  taskIds?: string[];
  sourceEventIds?: string[];
  payload?: Record<string, unknown>;
  waterfall?: {
    schema?: string;
    section?: "process" | "tool" | "subagent" | "final" | string;
    groupId?: string;
    displayRole?: string;
    collapsePolicy?: "collapse_after_final" | "never" | string;
    order?: number;
  };
  meta?: Record<string, unknown>;
  at?: number;
};

export type HeadlessTranscriptLine = {
  kind: string;
  section: string;
  title?: string;
  summary?: string;
  status?: string;
  text?: string;
  sourceItemId?: string;
  sourceEventId?: string;
  subagentId?: string;
  toolGroupId?: string;
  eventType?: string;
  indent: number;
};

export type TranscriptProjectionResponse = {
  lines: HeadlessTranscriptLine[];
};

export type AgentStreamPayload = {
  type?: string;
  agentRunId?: string;
  taskId?: string;
  cursor?: number;
  content?: string;
  message?: string;
  text?: string;
  stage?: string;
  sessionId?: string;
  turnId?: string;
  status?: string;
  startedAtMs?: number;
  updatedAtMs?: number;
  completedAtMs?: number | null;
  toolName?: string;
  callId?: string;
  providerItemId?: string;
  argsPreview?: string;
  argsJson?: string;
  delta?: string;
  finishReason?: string;
  elapsedMs?: number;
  event?: unknown;
  item?: unknown;
  segment?: unknown;
  items?: unknown[];
};

type StreamHandle = {
  close: () => void;
};

type DesktopAgentStreamCarrier = {
  sessionId?: string;
  agentRunId?: string;
  turnId?: string;
  streamItems?: unknown[];
};

type StreamBufferState = {
  items: AgentStreamPayload[];
  cursor: number;
};

type DesktopAgentEventEnvelope = {
  sessionId?: string;
  agentRunId?: string;
  payload?: unknown;
};

type NormalizedDesktopAgentEventEnvelope = {
  sessionId: string;
  agentRunId: string;
  payload: AgentStreamPayload;
};

const TERMINAL_SESSION_EVENT_TYPES = new Set([
  "AgentRunCompleted",
  "AgentRunFailed",
  "AgentRunInterrupted",
]);
const SUPPORTED_AGENT_STREAM_PAYLOAD_TYPES = new Set([
  "runtime_event",
  "session_event",
  "error",
]);

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null;

const desktopStreamBuffers = new Map<string, StreamBufferState>();
const desktopStreamConsumerCounts = new Map<string, number>();
const desktopStreamDrainers = new Map<string, Set<() => void>>();
let desktopAgentStreamSubscription: Promise<(() => void) | null> | null = null;
const DESKTOP_STREAM_MAX_PAYLOADS_PER_FRAME = 128;
const DESKTOP_STREAM_MAX_REDUCE_MS_PER_FRAME = 4;

const requireProtocolString = (value: unknown, field: string): string => {
  const normalized = typeof value === "string" ? value.trim() : "";
  if (!normalized) {
    throw new Error(`Agent stream protocol requires ${field}`);
  }
  return normalized;
};

const validateAgentStreamPayload = (value: unknown): AgentStreamPayload => {
  if (!isRecord(value)) {
    throw new Error("Agent stream protocol requires an object payload");
  }
  return value as AgentStreamPayload;
};

const normalizeDesktopStreamIdentity = (
  sessionIdValue: unknown,
  agentRunIdValue: unknown,
): { sessionId: string; agentRunId: string } => {
  const sessionId = requireProtocolString(sessionIdValue, "sessionId");
  const agentRunId = requireProtocolString(agentRunIdValue, "agentRunId");
  return { sessionId, agentRunId };
};

export const normalizeDesktopAgentEventEnvelope = (
  value: unknown,
): NormalizedDesktopAgentEventEnvelope => {
  if (!isRecord(value)) {
    throw new Error("Agent stream protocol requires an event envelope");
  }
  const { sessionId, agentRunId } = normalizeDesktopStreamIdentity(value.sessionId, value.agentRunId);
  return {
    sessionId,
    agentRunId,
    payload: validateAgentStreamPayload(value.payload),
  };
};

const attachStreamItems = (
  sessionId: string | undefined,
  agentRunId: string | undefined,
  rawItems: unknown[] | undefined,
  replace: boolean,
): void => {
  const { agentRunId: normalizedAgentRunId } = normalizeDesktopStreamIdentity(sessionId, agentRunId);
  const normalizedItems = Array.isArray(rawItems)
    ? rawItems.map(validateAgentStreamPayload)
    : [];
  if (!desktopStreamConsumerCounts.has(normalizedAgentRunId)) {
    return;
  }
  if (
    normalizedItems.length === 0 &&
    desktopStreamBuffers.has(normalizedAgentRunId)
  ) {
    return;
  }
  if (replace || !desktopStreamBuffers.has(normalizedAgentRunId)) {
    const nextBuffer: StreamBufferState = {
      items: [],
      cursor: 0,
    };
    for (const item of normalizedItems) {
      appendCoalescedStreamPayload(nextBuffer, item);
    }
    desktopStreamBuffers.set(normalizedAgentRunId, nextBuffer);
    notifyDesktopStreamDrainers(normalizedAgentRunId);
    return;
  }
  const buffer = desktopStreamBuffers.get(normalizedAgentRunId);
  if (!buffer) {
    return;
  }
  for (const item of normalizedItems) {
    appendCoalescedStreamPayload(buffer, item);
  }
  notifyDesktopStreamDrainers(normalizedAgentRunId);
};

const collectAgentResponse = <TResponse extends DesktopAgentStreamCarrier>(
  response: TResponse,
  replaceBuffer: boolean,
): TResponse => {
  attachStreamItems(
    response.sessionId,
    response.agentRunId,
    response.streamItems,
    replaceBuffer,
  );
  return response;
};

const pushDesktopStreamPayload = (
  agentRunId: string,
  payload: AgentStreamPayload,
): void => {
  const normalizedAgentRunId = agentRunId.trim();
  if (!normalizedAgentRunId) {
    return;
  }
  if (!desktopStreamConsumerCounts.has(normalizedAgentRunId)) {
    return;
  }
  const buffer = desktopStreamBuffers.get(normalizedAgentRunId) ?? {
    items: [],
    cursor: 0,
  };
  if (!desktopStreamBuffers.has(normalizedAgentRunId)) {
    desktopStreamBuffers.set(normalizedAgentRunId, buffer);
  }
  appendCoalescedStreamPayload(buffer, payload);
  notifyDesktopStreamDrainers(normalizedAgentRunId);
};

const isTerminalSessionEventPayload = (payload: AgentStreamPayload): boolean =>
  payload.type === "session_event" &&
  isRecord(payload.event) &&
  typeof payload.event.type === "string" &&
  TERMINAL_SESSION_EVENT_TYPES.has(payload.event.type);

const formatStreamPayloadType = (payload: AgentStreamPayload): string =>
  typeof payload.type === "string" && payload.type.trim()
    ? payload.type.trim()
    : "<missing>";

const isSupportedAgentStreamPayload = (payload: AgentStreamPayload): boolean =>
  SUPPORTED_AGENT_STREAM_PAYLOAD_TYPES.has(formatStreamPayloadType(payload));

const notifyDesktopStreamDrainers = (agentRunId: string): void => {
  const drainers = desktopStreamDrainers.get(agentRunId);
  if (!drainers) {
    return;
  }
  for (const scheduleDrain of drainers) {
    scheduleDrain();
  }
};

const registerDesktopStreamDrainer = (
  agentRunId: string,
  scheduleDrain: () => void,
): (() => void) => {
  let drainers = desktopStreamDrainers.get(agentRunId);
  if (!drainers) {
    drainers = new Set();
    desktopStreamDrainers.set(agentRunId, drainers);
  }
  drainers.add(scheduleDrain);
  return () => {
    const current = desktopStreamDrainers.get(agentRunId);
    if (!current) {
      return;
    }
    current.delete(scheduleDrain);
    if (current.size === 0) {
      desktopStreamDrainers.delete(agentRunId);
    }
  };
};

const retainDesktopStreamConsumer = (agentRunId: string): void => {
  const normalizedAgentRunId = agentRunId.trim();
  if (!normalizedAgentRunId) {
    return;
  }
  desktopStreamConsumerCounts.set(
    normalizedAgentRunId,
    (desktopStreamConsumerCounts.get(normalizedAgentRunId) ?? 0) + 1,
  );
};

const releaseDesktopStreamConsumer = (agentRunId: string): void => {
  const normalizedAgentRunId = agentRunId.trim();
  if (!normalizedAgentRunId) {
    return;
  }
  const current = desktopStreamConsumerCounts.get(normalizedAgentRunId) ?? 0;
  if (current <= 1) {
    desktopStreamConsumerCounts.delete(normalizedAgentRunId);
    desktopStreamBuffers.delete(normalizedAgentRunId);
    return;
  }
  desktopStreamConsumerCounts.set(normalizedAgentRunId, current - 1);
};

const ensureDesktopAgentStreamSubscription = async (): Promise<void> => {
  if (!isNativeHostRuntime()) {
    return;
  }
  if (!desktopAgentStreamSubscription) {
    desktopAgentStreamSubscription = (async () => {
      return listenHost<DesktopAgentEventEnvelope>("session/update", (event) => {
        const normalized = normalizeDesktopAgentEventEnvelope(event);
        pushDesktopStreamPayload(normalized.agentRunId, normalized.payload);
      });
    })();
  }
  await desktopAgentStreamSubscription;
};

export const getChatApiBaseUrl = (): string => "desktop://runtime";

export const listSessions = async (): Promise<SessionItem[]> => {
  if (!isNativeHostRuntime()) {
    throw new Error("native session list is desktop-only in Rust mainline");
  }
  return invokeHost<SessionItem[]>("session/list", {
    request: {},
  });
};

export const getSession = async (
  sessionId: string,
): Promise<SessionData> => {
  if (!isNativeHostRuntime()) {
    throw new Error("native session is desktop-only in Rust mainline");
  }
  return invokeHost<SessionData>("session/load", {
    request: {
      sessionId,
    },
  });
};

export const getSessionProjection = async (
  sessionId: string,
): Promise<SessionProjectionData> => {
  if (!isNativeHostRuntime()) {
    throw new Error("native session projection is desktop-only in Rust mainline");
  }
  return invokeHost<SessionProjectionData>("_centaeris/session/project", {
    request: {
      sessionId,
    },
  });
};

export const createSession = async (
  title: string,
  cwd: string,
  operationId: string,
): Promise<SessionItem> => {
  if (!isNativeHostRuntime()) {
    throw new Error("native session is desktop-only in Rust mainline");
  }
  if (!cwd.trim()) {
    throw new Error("cwd is required for local session creation");
  }
  return invokeHost<SessionItem>("session/new", {
    request: {
      operationId,
      title,
      cwd,
    },
  });
};

export const activateSession = async (
  sessionId: string,
  selectedAtMs: number,
): Promise<SessionItem> => {
  if (!isNativeHostRuntime()) {
    throw new Error("native session activation is desktop-only in Rust mainline");
  }
  return invokeHost<SessionItem>("_centaeris/session/activate", {
    request: {
      sessionId,
      selectedAtMs,
    },
  });
};

export const updateSession = async (
  sessionId: string,
  patch: {
    title?: string;
    isPinned?: boolean;
    isUnread?: boolean;
  },
): Promise<SessionItem> => {
  if (!isNativeHostRuntime()) {
    throw new Error("native session update is desktop-only in Rust mainline");
  }
  return invokeHost<SessionItem>("_centaeris/session/update_metadata", {
    request: {
      sessionId,
      title: patch.title,
      isPinned: patch.isPinned,
      isUnread: patch.isUnread,
    },
  });
};

export const reorderSessions = async (
  section: SessionReorderSection,
  orderedSessionIds: string[],
): Promise<SessionItem[]> => {
  if (!isNativeHostRuntime()) {
    throw new Error("native session reorder is desktop-only in Rust mainline");
  }
  return invokeHost<SessionItem[]>("_centaeris/session/reorder", {
    request: {
      section,
      orderedSessionIds,
    },
  });
};

export const getAgentState = async (
  sessionId: string,
  includeRuntimeState: boolean = true,
): Promise<AgentStateSummary> => {
  if (!isNativeHostRuntime()) {
    throw new Error("agent state is desktop-only in Rust mainline");
  }
  return invokeHost<AgentStateSummary>("agent_state_get", {
    request: {
      sessionId,
      includeRuntimeState,
    },
  });
};

export const getAgentContextUsage = async (
  sessionId: string,
): Promise<AgentContextUsageSummary> => {
  if (!isNativeHostRuntime()) {
    throw new Error("agent context usage is desktop-only in Rust mainline");
  }
  return invokeHost<AgentContextUsageSummary>("agent_context_usage_get", {
    request: {
      sessionId,
    },
  });
};

export const compactAgentContext = async (
  sessionId: string,
): Promise<AgentContextCompactResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error("context compaction is desktop-only in Rust mainline");
  }
  return invokeHost<AgentContextCompactResponse>("_centaeris/session/compact_context", {
    request: { sessionId },
  });
};

export const sendAgentInput = async (
  request: SendAgentInputRequest,
): Promise<SendAgentInputResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error("agent input is desktop-only in Rust mainline");
  }
  await ensureDesktopAgentStreamSubscription();
  const response = await invokeHost<DesktopAgentStreamCarrier>(
    "session/prompt",
    {
      request: {
        operationId: request.operationId,
        sessionId: request.sessionId,
        message: request.message,
        tailPolicy: request.tailPolicy,
        rewriteTargetMessageId: request.rewriteTargetMessageId,
        rewriteExpectedTailMessageId: request.rewriteExpectedTailMessageId,
        autoContinueAfterResumeWait: request.autoContinueAfterResumeWait,
      },
    },
  );
  const normalized = collectAgentResponse(response, true);
  return {
    sessionId: normalized.sessionId,
    agentRunId: normalized.agentRunId,
    turnId: normalized.turnId,
  };
};

export const listAgentRuns = async (
  request: AgentRunListRequest = {},
): Promise<AgentRunListResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error("agent runs are desktop-only in Rust mainline");
  }
  return invokeHost<AgentRunListResponse>("_centaeris/session/agent-runs", {
    request,
  });
};

export const replayAgentRunStream = async (
  request: AgentRunStreamReplayRequest,
): Promise<AgentRunStreamReplayResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error(
      "agent run stream replay is desktop-only in Rust mainline",
    );
  }
  return invokeHost<AgentRunStreamReplayResponse>(
    "_centaeris/session/agent-runs/replay",
    {
      request,
    },
  );
};

export const attachAgentRun = async (
  request: AgentRunAttachRequest,
): Promise<AgentRunAttachResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error("agent run attach is desktop-only in Rust mainline");
  }
  return invokeHost<AgentRunAttachResponse>("_centaeris/session/agent-runs/attach", {
    request,
  });
};

export const detachAgentRun = async (
  request: AgentRunDetachRequest,
): Promise<AgentRunDetachResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error("agent run detach is desktop-only in Rust mainline");
  }
  return invokeHost<AgentRunDetachResponse>("_centaeris/session/agent-runs/detach", {
    request,
  });
};

export const cancelAgentRun = async (
  request: AgentRunCancelRequest,
): Promise<AgentRunCancelResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error("agent run cancel is desktop-only in Rust mainline");
  }
  return invokeHost<AgentRunCancelResponse>("_centaeris/session/agent-runs/cancel", {
    request,
  });
};

export const getAgentRuntimeConfig = async (): Promise<AgentRuntimeConfig> => {
  if (!isNativeHostRuntime()) {
    throw new Error("agent runtime config is desktop-only in Rust mainline");
  }
  return invokeHost<AgentRuntimeConfig>("agent_runtime_config_get", {
    request: {},
  });
};

export const resetAgentRuntimeConfig = async (): Promise<AgentRuntimeConfigResetResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error("agent runtime config is desktop-only in Rust mainline");
  }
  return invokeHost<AgentRuntimeConfigResetResponse>("agent_runtime_config_reset", {
    request: { confirm: true },
  });
};

export const deleteSession = async (
  sessionId: string,
): Promise<SessionDeleteResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error("native session deletion is desktop-only in Rust mainline");
  }
  if (!sessionId.trim()) {
    throw new Error("sessionId is required");
  }
  return invokeHost<SessionDeleteResponse>("_centaeris/session/delete", {
    request: { sessionId },
  });
};

export const setAgentRuntimeConfig = async (
  request: SetAgentRuntimeConfigRequest,
): Promise<AgentRuntimeConfig> => {
  if (!isNativeHostRuntime()) {
    throw new Error("agent runtime config is desktop-only in Rust mainline");
  }
  return invokeHost<AgentRuntimeConfig>("agent_runtime_config_set", {
    request: {
      bashPath: request.bashPath,
      autoContinueAfterResumeWait: request.autoContinueAfterResumeWait,
      agentTransportMode: request.agentTransportMode,
      modelProviderId: request.modelProviderId,
      model: request.model,
      modelThinkingMode: request.modelThinkingMode,
      modelApiKey: request.modelApiKey,
      clearModelApiKey: request.clearModelApiKey,
      customModelProviders: request.customModelProviders,
      toolParallelism: request.toolParallelism,
    },
  });
};

export const listPlugins = async (): Promise<PluginDescriptorV1[]> => {
  if (!isNativeHostRuntime()) {
    throw new Error("plugin list is desktop-only in Rust mainline");
  }
  return invokeHost<PluginDescriptorV1[]>("plugin/list", {
    request: {},
  });
};

export const getNativeMcpCatalog = async (): Promise<NativeMcpCatalog> => {
  if (!isNativeHostRuntime()) {
    throw new Error("MCP catalog is desktop-only in Rust mainline");
  }
  return invokeHost<NativeMcpCatalog>("mcp/catalog", {
    request: {},
  });
};

export const configureNativeMcp = async (request: {
  pluginName: string;
  serverId: string;
  bearerToken: string;
}): Promise<NativeMcpCatalog> => {
  if (!isNativeHostRuntime()) {
    throw new Error("MCP configuration is desktop-only in Rust mainline");
  }
  return invokeHost<NativeMcpCatalog>("mcp/configure", {
    request,
  });
};

export type AgentRuntimeModelTestResponse = {
  httpStatus?: number | null;
  latencyMs: number;
  outputPreview?: string | null;
  errorKeyword?: string | null;
};

export const testAgentRuntimeModel = async (request: {
  providerId: string;
  model: string;
}): Promise<AgentRuntimeModelTestResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error("model test is desktop-only in Rust mainline");
  }
  return invokeHost<AgentRuntimeModelTestResponse>("agent_runtime_model_test", {
    request,
  });
};

export const getPluginDetail = async (request: {
  id: string;
}): Promise<PluginDetailV1> => {
  if (!isNativeHostRuntime()) {
    throw new Error("plugin detail is desktop-only in Rust mainline");
  }
  return invokeHost<PluginDetailV1>("plugin/detail", {
    request,
  });
};

export const selectAndInstallPlugin = async (): Promise<PluginInstallSelectionResponseV1> => {
  if (!isNativeHostRuntime()) {
    throw new Error("plugin installation is desktop-only in Rust mainline");
  }
  return invokeHost<PluginInstallSelectionResponseV1>("plugin_select_install_path", {
    request: {},
  });
};

export const removePlugin = async (request: {
  id: string;
}): Promise<PluginRemoveResponseV1> => {
  if (!isNativeHostRuntime()) {
    throw new Error("plugin removal is desktop-only in Rust mainline");
  }
  return invokeHost<PluginRemoveResponseV1>("plugin/remove", {
    request,
  });
};

export const setPluginEnabled = async (request: {
  id: string;
  enabled: boolean;
}): Promise<PluginCatalogStateV1> => {
  if (!isNativeHostRuntime()) {
    throw new Error("plugin enablement is desktop-only in Rust mainline");
  }
  return invokeHost<PluginCatalogStateV1>("plugin/set_enabled", {
    request,
  });
};

export const reloadPlugins =
  async (): Promise<PluginCatalogStateV1> => {
    if (!isNativeHostRuntime()) {
      throw new Error("plugin reload is desktop-only in Rust mainline");
    }
    return invokeHost<PluginCatalogStateV1>("plugin/reload", {
      request: {},
    });
  };

export const getPluginSourceRef = async (request: {
  id: string;
}): Promise<PluginSourceRefV1> => {
  if (!isNativeHostRuntime()) {
    throw new Error("plugin source ref is desktop-only in Rust mainline");
  }
  return invokeHost<PluginSourceRefV1>("plugin/source_ref", {
    request,
  });
};

export const revealPluginSourceRef = async (request: {
  id: string;
}): Promise<PluginRevealSourceRefResponseV1> => {
  if (!isNativeHostRuntime()) {
    throw new Error("plugin source reveal is desktop-only in Rust mainline");
  }
  return invokeHost<PluginRevealSourceRefResponseV1>("plugin_reveal_source_ref", {
    request,
  });
};

export const listSkillSources = async (): Promise<SkillSourcesConfig> => {
  if (!isNativeHostRuntime()) {
    throw new Error("skill sources are desktop-only in Rust mainline");
  }
  return invokeHost<SkillSourcesConfig>("skill/source/list", {
    request: {},
  });
};

export const getSkillCatalog = async (
  cwd?: string | null,
): Promise<SkillCatalogSnapshot> => {
  if (!isNativeHostRuntime()) {
    throw new Error("skill catalog is desktop-only in Rust mainline");
  }
  return invokeHost<SkillCatalogSnapshot>("skill/catalog", {
    request: { cwd: cwd || null },
  });
};

export const reloadSkillCatalog = async (
  cwd?: string | null,
): Promise<SkillCatalogSnapshot> => {
  if (!isNativeHostRuntime()) {
    throw new Error("skill catalog reload is desktop-only in Rust mainline");
  }
  return invokeHost<SkillCatalogSnapshot>("skill/reload", {
    request: { cwd: cwd || null },
  });
};

export const getSkillDetail = async (request: {
  cwd?: string | null;
  skillId: string;
}): Promise<SkillDetail> => {
  if (!isNativeHostRuntime()) {
    throw new Error("skill detail is desktop-only in Rust mainline");
  }
  return invokeHost<SkillDetail>("skill/detail", {
    request: {
      cwd: request.cwd || null,
      skillId: request.skillId,
    },
  });
};

export const addSkillSource = async (request: {
  scope: "workspace" | "user";
  kind: SkillSourceKind;
  path: string;
  workspaceRoot?: string | null;
}): Promise<SkillSourcesConfig> => {
  if (!isNativeHostRuntime()) {
    throw new Error("skill source mutation is desktop-only in Rust mainline");
  }
  return invokeHost<SkillSourcesConfig>("skill/source/add", {
    request: {
      scope: request.scope,
      kind: request.kind,
      path: request.path,
      workspaceRoot: request.workspaceRoot || null,
    },
  });
};

export const removeSkillSource = async (
  sourceId: string,
): Promise<SkillSourcesConfig> => {
  if (!isNativeHostRuntime()) {
    throw new Error("skill source mutation is desktop-only in Rust mainline");
  }
  return invokeHost<SkillSourcesConfig>("skill/source/remove", {
    request: { sourceId },
  });
};

export const setSkillSourceEnabled = async (request: {
  sourceId: string;
  enabled: boolean;
}): Promise<SkillSourcesConfig> => {
  if (!isNativeHostRuntime()) {
    throw new Error("skill source mutation is desktop-only in Rust mainline");
  }
  return invokeHost<SkillSourcesConfig>("skill/source/set_enabled", {
    request,
  });
};

export const setSkillEnabled = async (request: {
  cwd?: string | null;
  skillId: string;
  enabled: boolean;
}): Promise<SkillCatalogSnapshot> => {
  if (!isNativeHostRuntime()) {
    throw new Error("skill mutation is desktop-only in Rust mainline");
  }
  return invokeHost<SkillCatalogSnapshot>("skill/set_enabled", {
    request: {
      cwd: request.cwd || null,
      skillId: request.skillId,
      enabled: request.enabled,
    },
  });
};

export const selectSkillSourcePath = async (
  kind: SkillSourceKind,
): Promise<SkillSourcePathSelection> => {
  if (!isNativeHostRuntime()) {
    throw new Error("skill source picker is desktop-only in Rust mainline");
  }
  return invokeHost<SkillSourcePathSelection>("skill_select_source_path", {
    request: { kind },
  });
};

export const revealSkillSource = async (
  sourceId: string,
): Promise<SkillSourceRevealResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error("skill source reveal is desktop-only in Rust mainline");
  }
  return invokeHost<SkillSourceRevealResponse>("skill_reveal_source", {
    request: { sourceId },
  });
};

export const collectRuntimeGarbage = async (
  request: RuntimeGarbageCollectRequest,
): Promise<RuntimeGarbageCollectResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error("runtime garbage collection is desktop-only in Rust mainline");
  }
  return invokeHost<RuntimeGarbageCollectResponse>(
    "agent_runtime_garbage_collect",
    {
      request,
    },
  );
};

export const listAgentRuntimeJobs = async (
  request: AgentRuntimeJobListRequest,
): Promise<AgentRuntimeJobListResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error("runtime jobs are desktop-only in Rust mainline");
  }
  return invokeHost<AgentRuntimeJobListResponse>("agent_runtime_job_list", {
    request,
  });
};

export const getAgentRuntimeJob = async (
  jobId: string,
): Promise<AgentRuntimeJobGetResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error("runtime job get is desktop-only in Rust mainline");
  }
  return invokeHost<AgentRuntimeJobGetResponse>("agent_runtime_job_get", {
    request: {
      jobId,
    },
  });
};

export const listAgentDeadLetters = async (
  request: AgentDeadLetterListRequest,
): Promise<AgentDeadLetterListResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error("dead letters are desktop-only in Rust mainline");
  }
  return invokeHost<AgentDeadLetterListResponse>("agent_dead_letter_list", {
    request,
  });
};

export const getAgentDeadLetter = async (
  deadLetterId: string,
): Promise<AgentDeadLetterGetResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error("dead letter get is desktop-only in Rust mainline");
  }
  return invokeHost<AgentDeadLetterGetResponse>("agent_dead_letter_get", {
    request: {
      deadLetterId,
    },
  });
};

export const dismissAgentDeadLetter = async (
  request: AgentDeadLetterDismissRequest,
): Promise<AgentDeadLetterActionResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error("dead letter dismiss is desktop-only in Rust mainline");
  }
  return invokeHost<AgentDeadLetterActionResponse>(
    "agent_dead_letter_dismiss",
    {
      request: {
        deadLetterId: request.deadLetterId,
        dismissedBy: request.dismissedBy,
        dismissedReason: request.dismissedReason,
      },
    },
  );
};

export const replayAgentDeadLetter = async (
  request: AgentDeadLetterReplayRequest,
): Promise<AgentDeadLetterActionResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error("dead letter replay is desktop-only in Rust mainline");
  }
  return invokeHost<AgentDeadLetterActionResponse>(
    "agent_dead_letter_replay",
    {
      request: {
        deadLetterId: request.deadLetterId,
        replayKey: request.replayKey,
        jobId: request.jobId,
        idempotencyKey: request.idempotencyKey,
        runAtMs: request.runAtMs,
        maxRetries: request.maxRetries,
      },
    },
  );
};

export const projectTranscript = async (
  streamItems: AgentStreamPayload[],
): Promise<TranscriptProjectionResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error(
      "headless session_event projection is desktop-only in Rust mainline",
    );
  }
  return invokeHost<TranscriptProjectionResponse>(
    "transcript/project",
    {
      request: {
        streamItems,
      },
    },
  );
};

export const listenAgentRuntimeConfigChanges = async (
  handler: () => void,
): Promise<() => void> => {
  if (!isNativeHostRuntime()) {
    return () => undefined;
  }
  return listenHost<unknown>("runtime/config-changed", (payload) => {
    if (!isRecord(payload) || Object.keys(payload).length !== 0) {
      throw new Error("runtime/config-changed payload must be an empty object");
    }
    handler();
  });
};

export const sendAgentSupplement = async (
  request: SendAgentSupplementRequest,
): Promise<SendAgentSupplementResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error("agent supplement is desktop-only in Rust mainline");
  }
  await ensureDesktopAgentStreamSubscription();
  const response = await invokeHost<
    DesktopAgentStreamCarrier & SendAgentSupplementResponse
  >("_centaeris/session/supplement", {
    request: {
      sessionId: request.sessionId,
      agentRunId: request.agentRunId,
      message: request.message,
    },
  });
  const normalized = collectAgentResponse(response, false);
  return {
    accepted: normalized.accepted,
    sessionId: normalized.sessionId,
    agentRunId: normalized.agentRunId,
    queuedCount: normalized.queuedCount,
  };
};

export const answerAgentQuestion = async (
  request: AnswerAgentQuestionRequest,
): Promise<AgentInitResponse> => {
  if (!isNativeHostRuntime()) {
    throw new Error("agent question is desktop-only in Rust mainline");
  }
  await ensureDesktopAgentStreamSubscription();
  const response = await invokeHost<DesktopAgentStreamCarrier>(
    "_centaeris/session/answer_question",
    {
      request: {
        sessionId: request.sessionId,
        questionId: request.questionId,
        answers: request.answers,
        answerText: request.answerText,
        autoContinueAfterResumeWait: request.autoContinueAfterResumeWait,
      },
    },
  );
  const normalized = collectAgentResponse(response, true);
  return {
    sessionId: normalized.sessionId,
    agentRunId: normalized.agentRunId,
    turnId: normalized.turnId,
  };
};

export const openAgentStream = (
  agentRunId: string,
  onMessage: (payload: AgentStreamPayload) => void,
  onError?: (error: Error) => void,
  onOpen?: () => void,
): StreamHandle => {
  const normalizedAgentRunId = agentRunId.trim();
  if (!normalizedAgentRunId) {
    onError?.(new Error("Agent stream agentRunId is required"));
    return {
      close: () => undefined,
    };
  }

  if (!isNativeHostRuntime()) {
    onError?.(new Error("Agent stream is desktop-only in Rust mainline"));
    return {
      close: () => undefined,
    };
  }

  retainDesktopStreamConsumer(normalizedAgentRunId);
  void ensureDesktopAgentStreamSubscription()
    .then(() => {
      onOpen?.();
    })
    .catch((error: unknown) => {
      onError?.(
        error instanceof Error
          ? error
          : new Error("failed to subscribe desktop agent stream"),
      );
      closeConsumer();
    });
  let closed = false;
  let lastActivityAt = Date.now();
  let drainScheduled = false;
  let idleTimer: number | null = null;
  let unregisterDrainer: (() => void) | null = null;
  const maxIdleMs = 30 * 60 * 1000;
  const failIdleTimeout = (): void => {
    onError?.(
      new Error(
        `Agent stream idle timeout before terminal session_event: ${normalizedAgentRunId}`,
      ),
    );
    closeConsumer();
  };
  const resetIdleTimer = (): void => {
    if (idleTimer !== null) {
      window.clearTimeout(idleTimer);
    }
    idleTimer = window.setTimeout(() => {
      if (!closed && Date.now() - lastActivityAt >= maxIdleMs) {
        failIdleTimeout();
      }
    }, maxIdleMs);
  };
  const closeConsumer = (): void => {
    if (closed) {
      return;
    }
    closed = true;
    if (idleTimer !== null) {
      window.clearTimeout(idleTimer);
      idleTimer = null;
    }
    unregisterDrainer?.();
    unregisterDrainer = null;
    releaseDesktopStreamConsumer(normalizedAgentRunId);
  };
  const drainBuffer = (): void => {
    drainScheduled = false;
    if (closed) {
      return;
    }
    const buffer = desktopStreamBuffers.get(normalizedAgentRunId);
    if (!buffer) {
      return;
    }
    if (buffer.cursor >= buffer.items.length) {
      return;
    }
    let processed = 0;
    const startedAt =
      typeof performance !== "undefined" &&
      typeof performance.now === "function"
        ? performance.now()
        : Date.now();
    while (
      buffer.cursor < buffer.items.length &&
      processed < DESKTOP_STREAM_MAX_PAYLOADS_PER_FRAME
    ) {
      if (processed > 0) {
        const now =
          typeof performance !== "undefined" &&
          typeof performance.now === "function"
            ? performance.now()
            : Date.now();
        if (now - startedAt >= DESKTOP_STREAM_MAX_REDUCE_MS_PER_FRAME) {
          break;
        }
      }
      lastActivityAt = Date.now();
      const nextPayload = buffer.items[buffer.cursor];
      buffer.cursor += 1;
      processed += 1;
      if (!isSupportedAgentStreamPayload(nextPayload)) {
        onError?.(
          new Error(
            `Agent stream received unsupported payload type: ${formatStreamPayloadType(nextPayload)}`,
          ),
        );
        closeConsumer();
        return;
      }
      onMessage(nextPayload);
      resetIdleTimer();
      if (isTerminalSessionEventPayload(nextPayload)) {
        closeConsumer();
        return;
      }
    }
    compactConsumedStreamPayloads(buffer);
    if (buffer.cursor < buffer.items.length) {
      scheduleDrain();
    }
  };
  const scheduleDrain = (): void => {
    if (closed || drainScheduled) {
      return;
    }
    drainScheduled = true;
    if (typeof window.requestAnimationFrame === "function") {
      window.requestAnimationFrame(drainBuffer);
      return;
    }
    window.setTimeout(drainBuffer, 16);
  };
  unregisterDrainer = registerDesktopStreamDrainer(
    normalizedAgentRunId,
    scheduleDrain,
  );
  resetIdleTimer();
  scheduleDrain();

  return {
    close: () => {
      closeConsumer();
    },
  };
};
