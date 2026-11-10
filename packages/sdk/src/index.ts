import type {
  AgentRun,
  AgentRunSummary,
  AnswerNowReceipt,
  AnswerNowRequest,
  CancelReceipt,
  CancelRequest,
  ClientRegistration,
  CompactContextReceipt,
  CompactContextRequest,
  CreateSessionRequest,
  ListRunsRequest,
  LoadSessionRequest,
  PersistedMessage,
  QuestionAnswerRequest,
  RpcTransport,
  RunList,
  RunSubscription,
  RunTarget,
  RuntimeDescriptor,
  SendReceipt,
  SendRequest,
  SessionCancelReceipt,
  SessionData,
  SessionItem,
  SessionPrompt,
  SessionQuestionAnswer,
  SessionUpdate,
  SubscriptionReceipt,
  SupplementReceipt,
  SupplementRequest,
} from "./types.ts";

export type * from "./types.ts";

export class RpcError extends Error {
  readonly code: number;
  readonly data: unknown;

  constructor(code: number, message: string, data?: unknown) {
    super(message);
    this.name = "RpcError";
    this.code = code;
    this.data = data;
  }
}

export class RpcProtocolError extends Error {
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "RpcProtocolError";
  }
}

export class RpcConnectionClosedError extends Error {
  constructor(message = "RPC connection closed", options?: ErrorOptions) {
    super(message, options);
    this.name = "RpcConnectionClosedError";
  }
}

interface PendingRequest {
  resolve(value: unknown): void;
  reject(error: Error): void;
}

interface RpcNotification {
  method: string;
  params: unknown;
}

interface RpcResponse {
  id: unknown;
  result?: unknown;
  error?: { code: number; message: string; data?: unknown };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function exactFields(
  value: Record<string, unknown>,
  required: string[],
  optional: string[] = [],
): boolean {
  return required.every((field) => Object.hasOwn(value, field))
    && Object.keys(value).every((field) => required.includes(field) || optional.includes(field));
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function isNullableString(value: unknown): value is string | null {
  return value === null || typeof value === "string";
}

function isUnsignedInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0;
}

function isNullableInteger(value: unknown): value is number | null {
  return value === null || Number.isInteger(value);
}

function decodeDescriptor(value: unknown): RuntimeDescriptor {
  if (!isRecord(value) || !exactFields(value, [
    "status", "runtime", "protocol", "protocolVersion", "capabilities", "events",
    "projections", "buildId", "coreProtocolVersion", "profileId", "storeId",
    "storeSchemaVersion", "layoutSchemaVersion",
  ]) || value.status !== "ok" || value.runtime !== "centaeris-runtime"
    || value.protocol !== "centaeris.runtime" || value.protocolVersion !== 1
    || !["buildId", "coreProtocolVersion", "profileId", "storeId"]
      .every((field) => typeof value[field] === "string")
    || ![value.capabilities, value.events, value.projections].every(isStringArray)
    || !Number.isInteger(value.storeSchemaVersion)
    || !isUnsignedInteger(value.layoutSchemaVersion) || value.layoutSchemaVersion > 0xffff_ffff) {
    throw new RpcProtocolError("Invalid initialize result or unsupported Runtime descriptor");
  }
  return value as unknown as RuntimeDescriptor;
}

function decodeSessionItem(value: unknown): SessionItem {
  if (!isRecord(value) || !exactFields(value, [
    "id", "title", "updatedAt", "lastMessage", "cwd", "sessionKind",
    "parentSessionId", "runtimeJobId", "sortOrder", "isPinned", "isUnread",
    "messageCount", "activityState",
  ]) || !["id", "title", "sessionKind", "activityState"]
    .every((field) => typeof value[field] === "string")
    || ![value.lastMessage, value.cwd, value.parentSessionId, value.runtimeJobId].every(isNullableString)
    || !Number.isInteger(value.updatedAt) || !Number.isInteger(value.sortOrder)
    || typeof value.isPinned !== "boolean" || typeof value.isUnread !== "boolean"
    || !isUnsignedInteger(value.messageCount)) {
    throw new RpcProtocolError("Invalid session/new result");
  }
  return value as unknown as SessionItem;
}

function isPersistedMessage(value: unknown): value is PersistedMessage {
  return isRecord(value) && exactFields(value, [
    "id", "sessionId", "turnId", "role", "content", "createdAtMs", "updatedAtMs",
  ], ["status", "agentRunId", "imageData"])
    && ["id", "sessionId", "turnId", "role", "content"]
      .every((field) => typeof value[field] === "string")
    && Number.isInteger(value.createdAtMs) && Number.isInteger(value.updatedAtMs)
    && (!Object.hasOwn(value, "status") || typeof value.status === "string")
    && (!Object.hasOwn(value, "agentRunId") || typeof value.agentRunId === "string");
}

function decodeSessionData(value: unknown): SessionData {
  if (!isRecord(value) || !exactFields(value, [
    "id", "title", "createdAt", "updatedAt", "sessionKind", "parentSessionId",
    "runtimeJobId", "messages",
  ]) || !["id", "title", "sessionKind"].every((field) => typeof value[field] === "string")
    || !Number.isInteger(value.createdAt) || !Number.isInteger(value.updatedAt)
    || !isNullableString(value.parentSessionId) || !isNullableString(value.runtimeJobId)
    || !Array.isArray(value.messages) || !value.messages.every(isPersistedMessage)) {
    throw new RpcProtocolError("Invalid session/load result");
  }
  return value as unknown as SessionData;
}

function decodeSendReceipt(value: unknown): SendReceipt {
  if (!isRecord(value) || !exactFields(value, ["sessionId", "agentRunId", "turnId", "streamItems"])
    || !["sessionId", "agentRunId", "turnId"].every((field) => typeof value[field] === "string")
    || !Array.isArray(value.streamItems)) {
    throw new RpcProtocolError("Invalid Session admission result");
  }
  return value as unknown as SendReceipt;
}

function isAgentRunSummary(value: unknown): value is AgentRunSummary {
  return isRecord(value) && exactFields(value, [
    "agentRunId", "sessionId", "turnId", "agentRunKind", "cwd", "status", "unread",
    "startedAtMs", "updatedAtMs", "completedAtMs", "lastEventAtMs", "stallReason",
    "watchdog", "error",
  ]) && ["agentRunId", "sessionId", "turnId", "agentRunKind", "status"]
    .every((field) => typeof value[field] === "string")
    && [value.cwd, value.stallReason, value.error].every(isNullableString)
    && typeof value.unread === "boolean"
    && Number.isInteger(value.startedAtMs) && Number.isInteger(value.updatedAtMs)
    && isNullableInteger(value.completedAtMs) && isNullableInteger(value.lastEventAtMs);
}

function decodeSubscriptionReceipt(value: unknown): SubscriptionReceipt {
  if (!isRecord(value) || !exactFields(value, [
    "agentRun", "viewerId", "transitionReason", "attachedViewerCount",
  ]) || (value.agentRun !== null && !isAgentRunSummary(value.agentRun))
    || typeof value.viewerId !== "string" || typeof value.transitionReason !== "string"
    || !isUnsignedInteger(value.attachedViewerCount)) {
    throw new RpcProtocolError("Invalid agent-runs/attach result");
  }
  return value as unknown as SubscriptionReceipt;
}

function decodeCancelReceipt(value: unknown): CancelReceipt {
  if (!isRecord(value) || !exactFields(value, ["agentRun", "cancelAccepted"])
    || (value.agentRun !== null && !isAgentRunSummary(value.agentRun))
    || typeof value.cancelAccepted !== "boolean") {
    throw new RpcProtocolError("Invalid agent-runs/cancel result");
  }
  return value as unknown as CancelReceipt;
}

function decodeSupplementReceipt(value: unknown): SupplementReceipt {
  if (!isRecord(value) || !exactFields(value, ["accepted", "sessionId", "agentRunId", "queuedCount"])
    || typeof value.accepted !== "boolean"
    || typeof value.sessionId !== "string" || typeof value.agentRunId !== "string"
    || !isUnsignedInteger(value.queuedCount)) {
    throw new RpcProtocolError("Invalid session/supplement result");
  }
  return value as unknown as SupplementReceipt;
}

function decodeAnswerNowReceipt(value: unknown): AnswerNowReceipt {
  if (!isRecord(value) || !exactFields(value, [
    "accepted", "disposition", "sessionId", "agentRunId", "interventionId",
  ]) || typeof value.accepted !== "boolean"
    || (value.disposition !== "accepted" && value.disposition !== "alreadyConverging")
    || ![value.sessionId, value.agentRunId, value.interventionId]
      .every((field) => typeof field === "string")) {
    throw new RpcProtocolError("Invalid session/answer_now result");
  }
  return value as unknown as AnswerNowReceipt;
}

function decodeCompactContextReceipt(value: unknown): CompactContextReceipt {
  if (!isRecord(value) || !exactFields(value, ["sessionId", "compacted"])
    || typeof value.sessionId !== "string" || typeof value.compacted !== "boolean") {
    throw new RpcProtocolError("Invalid session/compact_context result");
  }
  return value as unknown as CompactContextReceipt;
}

function decodeRunList(value: unknown): RunList {
  if (!isRecord(value) || !exactFields(value, ["agentRuns"])
    || !Array.isArray(value.agentRuns) || !value.agentRuns.every(isAgentRunSummary)) {
    throw new RpcProtocolError("Invalid session/agent-runs result");
  }
  return value as unknown as RunList;
}

function decodeFrame(text: string): RpcNotification | RpcResponse {
  const value: unknown = JSON.parse(text);
  if (!isRecord(value) || value.jsonrpc !== "2.0") {
    throw new RpcProtocolError("Expected a JSON-RPC 2.0 object");
  }
  if (Object.hasOwn(value, "method")) {
    if (!exactFields(value, ["jsonrpc", "method"], ["params"])
      || typeof value.method !== "string" || !value.method
      || value.method.trim() !== value.method) {
      throw new RpcProtocolError("Invalid JSON-RPC notification");
    }
    return { method: value.method, params: value.params ?? null };
  }
  const hasResult = Object.hasOwn(value, "result");
  const hasError = Object.hasOwn(value, "error");
  if (!exactFields(value, ["jsonrpc", "id"], ["result", "error"])
    || hasResult === hasError) {
    throw new RpcProtocolError("Response requires exactly one result or error");
  }
  if (hasError) {
    const error = value.error;
    if (!isRecord(error) || !exactFields(error, ["code", "message"], ["data"])
      || !Number.isInteger(error.code) || typeof error.message !== "string") {
      throw new RpcProtocolError("Invalid JSON-RPC error");
    }
    return { id: value.id, error: { code: error.code as number, message: error.message, data: error.data } };
  }
  return { id: value.id, result: value.result };
}

function updateEnvelope(params: unknown): SessionUpdate {
  const outer = isRecord(params) ? params : {};
  const event = isRecord(outer.event) ? outer.event : {};
  const identity = (key: string) => {
    const value = outer[key] ?? event[key];
    return typeof value === "string" ? value : undefined;
  };
  return {
    params,
    sessionId: identity("sessionId"),
    agentRunId: identity("agentRunId"),
    turnId: identity("turnId"),
  };
}

/** A connection-scoped client. The Runtime owns accepted Runs after disconnect. */
export class SessionClient {
  readonly #transport: RpcTransport;
  readonly #viewerId: string;
  readonly #pending = new Map<string, PendingRequest>();
  readonly #updates = new Set<(update: SessionUpdate) => void>();
  #nextId = 0n;
  #closed: Error | undefined;
  #unsubscribe: (() => void) | undefined;
  #descriptor!: RuntimeDescriptor;

  private constructor(transport: RpcTransport, viewerId: string) {
    this.#transport = transport;
    this.#viewerId = viewerId;
    this.#unsubscribe = transport.listen({
      message: (frame) => this.#receive(frame),
      close: (cause) => this.#finish(cause instanceof RpcProtocolError
        ? cause
        : new RpcConnectionClosedError("RPC transport closed", { cause })),
    });
  }

  /** Register this connection's explicit viewer before using Session operations. */
  static async connect(transport: RpcTransport, registration: ClientRegistration): Promise<SessionClient> {
    const client = new SessionClient(transport, registration.viewerId);
    try {
      client.#descriptor = await client.#request("initialize", {
        clientKind: registration.clientKind ?? "sdk",
        viewerId: registration.viewerId,
      }, decodeDescriptor);
      return client;
    } catch (error) {
      client.close();
      throw error;
    }
  }

  get descriptor(): RuntimeDescriptor {
    return this.#descriptor;
  }

  create(request: CreateSessionRequest): Promise<SessionItem> {
    return this.#request("session/new", request, decodeSessionItem);
  }

  load(request: LoadSessionRequest): Promise<SessionData> {
    return this.#request("session/load", request, decodeSessionData);
  }

  /** Resolve with durable admission. Completion arrives through session/update. */
  send(request: SendRequest): Promise<SendReceipt> {
    return this.#request("session/prompt", request, decodeSendReceipt);
  }

  /** Attach this connection's registered viewer; updates use onUpdate. */
  subscribe(request: RunTarget): Promise<SubscriptionReceipt> {
    return this.#request("_centaeris/session/agent-runs/attach", {
      ...request,
      viewerId: this.#viewerId,
    }, decodeSubscriptionReceipt);
  }

  /** Resolve with cancellation admission. Observe the Run's later terminal event. */
  cancel(request: CancelRequest): Promise<CancelReceipt> {
    return this.#request("_centaeris/session/agent-runs/cancel", request, decodeCancelReceipt);
  }

  supplement(request: SupplementRequest): Promise<SupplementReceipt> {
    return this.#request("_centaeris/session/supplement", request, decodeSupplementReceipt);
  }

  answerNow(request: AnswerNowRequest): Promise<AnswerNowReceipt> {
    return this.#request("_centaeris/session/answer_now", request, decodeAnswerNowReceipt);
  }

  answerQuestion(request: QuestionAnswerRequest): Promise<SendReceipt> {
    return this.#request("_centaeris/session/answer_question", request, decodeSendReceipt);
  }

  compactContext(request: CompactContextRequest): Promise<CompactContextReceipt> {
    return this.#request("_centaeris/session/compact_context", request, decodeCompactContextReceipt);
  }

  listRuns(request: ListRunsRequest = {}): Promise<RunList> {
    return this.#request("_centaeris/session/agent-runs", request, decodeRunList);
  }

  async createSession(request: CreateSessionRequest): Promise<AgentSession> {
    const created = await this.create(request);
    return new AgentSession(this, created.id);
  }

  async openSession(sessionId: string): Promise<AgentSession> {
    const loaded = await this.load({ sessionId });
    if (loaded.id !== sessionId) throw new RpcProtocolError("session/load returned a different Session");
    return new AgentSession(this, loaded.id);
  }

  /** Load durable history and reattach this viewer to the existing Run. */
  async resumeSession(sessionId: string): Promise<{ session: AgentSession; subscription: RunSubscription }> {
    const session = await this.openSession(sessionId);
    const subscription = await session.subscribe();
    return { session, subscription };
  }

  /** Listen before sending when output can precede the admission response. */
  onUpdate(listener: (update: SessionUpdate) => void): () => void {
    if (this.#closed) throw this.#closed;
    this.#updates.add(listener);
    return () => this.#updates.delete(listener);
  }

  /** Close this connection and reject pending requests. */
  close(): void {
    this.#finish(new RpcConnectionClosedError());
  }

  #request<Result>(method: string, request: unknown, decode: (value: unknown) => Result): Promise<Result> {
    if (this.#closed) return Promise.reject(this.#closed);
    const id = `sdk-${++this.#nextId}`;
    return new Promise<Result>((resolve, reject) => {
      this.#pending.set(id, { resolve: (value) => resolve(decode(value)), reject });
      try {
        const sent = this.#transport.send(JSON.stringify({
          jsonrpc: "2.0", id, method, params: { request },
        }));
        Promise.resolve(sent).catch((cause: unknown) => this.#sendFailed(cause));
      } catch (cause) {
        this.#sendFailed(cause);
      }
    });
  }

  #sendFailed(cause: unknown): void {
    this.#finish(new RpcConnectionClosedError("RPC transport send failed", { cause }));
  }

  #receive(text: string): void {
    if (this.#closed) return;
    let frame: RpcNotification | RpcResponse;
    try {
      frame = decodeFrame(text);
    } catch (cause) {
      this.#finish(cause instanceof RpcProtocolError
        ? cause
        : new RpcProtocolError("Invalid JSON-RPC JSON", { cause }));
      return;
    }
    if ("method" in frame) {
      if (frame.method === "session/update") {
        const update = updateEnvelope(frame.params);
        for (const listener of this.#updates) listener(update);
      }
      return;
    }
    const pending = typeof frame.id === "string" ? this.#pending.get(frame.id) : undefined;
    if (!pending || typeof frame.id !== "string") {
      this.#finish(new RpcProtocolError("Response has no matching request ID"));
      return;
    }
    this.#pending.delete(frame.id);
    if (frame.error) {
      pending.reject(new RpcError(frame.error.code, frame.error.message, frame.error.data));
    } else {
      try {
        pending.resolve(frame.result);
      } catch (cause) {
        const error = cause instanceof RpcProtocolError
          ? cause
          : new RpcProtocolError("Invalid RPC result", { cause });
        pending.reject(error);
        this.#finish(error);
      }
    }
  }

  #finish(error: Error): void {
    if (this.#closed) return;
    this.#closed = error;
    this.#unsubscribe?.();
    this.#updates.clear();
    for (const pending of this.#pending.values()) pending.reject(error);
    this.#pending.clear();
    this.#transport.close();
  }
}

function bindSessionRequest<Request extends object>(
  sessionId: string,
  request: Request,
  allowedFields: string[],
): Request & { sessionId: string } {
  if (!isRecord(request) || !exactFields(request, [], allowedFields)) {
    throw new TypeError("Bound Session request contains unsupported fields");
  }
  return { ...request, sessionId };
}

function requiredIdentity(value: unknown, field: string): string {
  if (typeof value !== "string" || !value.trim()) {
    throw new TypeError(`${field} must be a non-empty string`);
  }
  return value;
}

function toAgentRun(run: AgentRunSummary): AgentRun {
  return {
    agentRunId: run.agentRunId,
    sessionId: run.sessionId,
    turnId: run.turnId,
    agentRunKind: run.agentRunKind,
    cwd: run.cwd,
    status: run.status,
    startedAtMs: run.startedAtMs,
    updatedAtMs: run.updatedAtMs,
    completedAtMs: run.completedAtMs,
    lastEventAtMs: run.lastEventAtMs,
    stallReason: run.stallReason,
    watchdog: run.watchdog,
    error: run.error,
  };
}

/** Session operations share their client's connection, viewer and event stream. */
export class AgentSession {
  readonly #client: SessionClient;
  readonly #id: string;

  constructor(client: SessionClient, sessionId: string) {
    this.#client = client;
    this.#id = requiredIdentity(sessionId, "sessionId");
  }

  get id(): string {
    return this.#id;
  }

  load(): Promise<SessionData> {
    return this.#client.load({ sessionId: this.#id });
  }

  send(prompt: SessionPrompt): Promise<SendReceipt> {
    return this.#client.send(bindSessionRequest(this.#id, prompt, [
      "operationId", "message", "tailPolicy", "rewriteTargetMessageId",
      "rewriteExpectedTailMessageId", "autoContinueAfterResumeWait", "attachments",
    ]));
  }

  async subscribe(agentRunId?: string): Promise<RunSubscription> {
    const attached = await this.#client.subscribe({
      sessionId: this.#id,
      agentRunId: agentRunId === undefined ? undefined : requiredIdentity(agentRunId, "agentRunId"),
    });
    return { ...attached, agentRun: attached.agentRun && toAgentRun(attached.agentRun) };
  }

  async cancel(agentRunId: string, reason?: string): Promise<SessionCancelReceipt> {
    const cancelled = await this.#client.cancel({
      sessionId: this.#id, agentRunId: requiredIdentity(agentRunId, "agentRunId"), reason,
    });
    return { ...cancelled, agentRun: cancelled.agentRun && toAgentRun(cancelled.agentRun) };
  }

  supplement(agentRunId: string, message: string): Promise<SupplementReceipt> {
    return this.#client.supplement({
      sessionId: this.#id, agentRunId: requiredIdentity(agentRunId, "agentRunId"), message,
    });
  }

  answerQuestion(answer: SessionQuestionAnswer): Promise<SendReceipt> {
    return this.#client.answerQuestion(bindSessionRequest(this.#id, answer, [
      "questionId", "answers", "answerText", "autoContinueAfterResumeWait",
    ]));
  }

  async answerNow(intervention: AnswerNowRequest): Promise<AnswerNowReceipt> {
    if (!isRecord(intervention) || !exactFields(intervention, [
      "schema", "interventionId", "agentRunId", "kind",
    ])) {
      throw new TypeError("Answer-now intervention contains unsupported fields");
    }
    const runs = await this.listRuns(true);
    if (!runs.some((run) => run.agentRunId === intervention.agentRunId && run.sessionId === this.#id)) {
      throw new TypeError("agentRunId does not identify a Run in this Session");
    }
    return this.#client.answerNow(intervention);
  }

  compactContext(): Promise<CompactContextReceipt> {
    return this.#client.compactContext({ sessionId: this.#id });
  }

  async listRuns(includeTerminal?: boolean): Promise<AgentRun[]> {
    const runs = await this.#client.listRuns({ sessionId: this.#id, includeTerminal });
    return runs.agentRuns.map(toAgentRun);
  }
}
