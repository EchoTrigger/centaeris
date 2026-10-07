import type { StreamEntry, TerminalSessionEventType } from "./streamTypes.ts";
import type { ChatViewStore, StreamTelemetry } from "./chatViewStore.ts";

const DEFAULT_MAX_ITEMS_PER_FRAME = 128;
const DEFAULT_REDUCER_BUDGET_MS = 4;
const DEFAULT_QUEUE_HIGH_WATER_MARK = 512;
const TERMINAL_EVENT_TYPES = [
  "agent_run_completed",
  "agent_run_failed",
  "agent_run_interrupted",
] as const satisfies readonly TerminalSessionEventType[];
const TERMINALS: ReadonlySet<string> = new Set(TERMINAL_EVENT_TYPES);

type IdleWaiter = {
  resolve(): void;
  reject(reason: Error): void;
};

type WorkspaceChatControllerOptions = {
  store: ChatViewStore;
  workspaceId: string;
  sessionId: string;
  agentRunId: string;
  initialCursor?: string;
  maxItemsPerFrame?: number;
  reducerBudgetMs?: number;
  queueHighWaterMark?: number;
  scheduleFrame?: (callback: () => void) => number;
  cancelFrame?: (frameId: number) => void;
  now?: () => number;
  onAgentRunError?: (payload: unknown) => void;
  onCitationsChanged?: () => void;
};

function canCoalesce(left: StreamEntry | undefined, right: StreamEntry) {
  return left?.item.kind === "live"
    && right?.item.kind === "live"
    && left.item.agentRunId === right.item.agentRunId
    && left.item.messageId === right.item.messageId;
}

export function coalesceAdjacentStreamEntries(
  entries: readonly StreamEntry[],
) {
  const result: StreamEntry[] = [];
  for (const entry of entries) {
    if (canCoalesce(result.at(-1), entry)) result[result.length - 1] = entry;
    else result.push(entry);
  }
  return result;
}

export class WorkspaceChatController {
  readonly store: ChatViewStore;
  readonly workspaceId: string;
  readonly sessionId: string;
  readonly agentRunId: string;
  lastCursor: string;
  receivedCursor: string;
  readonly maxItemsPerFrame: number;
  readonly reducerBudgetMs: number;
  readonly queueHighWaterMark: number;
  readonly scheduleFrame: (callback: () => void) => number;
  readonly cancelFrame: (frameId: number) => void;
  readonly now: () => number;
  readonly onAgentRunError: (payload: unknown) => void;
  readonly onCitationsChanged: () => void;
  private queue: StreamEntry[];
  private queueOffset: number;
  private frameId: number | null;
  private disposed: boolean;
  private terminalAccepted: boolean;
  private failure: Error | null;
  private idleWaiters: IdleWaiter[];
  private acceptedTelemetry: WeakMap<StreamEntry, Readonly<StreamTelemetry>>;
  private acceptedOrdinal: number;

  constructor({
    store,
    workspaceId,
    sessionId,
    agentRunId,
    initialCursor = "0-0",
    maxItemsPerFrame = DEFAULT_MAX_ITEMS_PER_FRAME,
    reducerBudgetMs = DEFAULT_REDUCER_BUDGET_MS,
    queueHighWaterMark = DEFAULT_QUEUE_HIGH_WATER_MARK,
    scheduleFrame = (callback) => requestAnimationFrame(callback),
    cancelFrame = (frameId) => cancelAnimationFrame(frameId),
    now = () => performance.now(),
    onAgentRunError = () => {},
    onCitationsChanged = () => {},
  }: WorkspaceChatControllerOptions) {
    if (!store || !workspaceId || !sessionId || !agentRunId) throw new Error("chat controller identity is required");
    if (typeof initialCursor !== "string" || !initialCursor) throw new Error("initial stream cursor is invalid");
    if (!Number.isInteger(maxItemsPerFrame) || maxItemsPerFrame < 1) throw new Error("max items per frame is invalid");
    if (!Number.isFinite(reducerBudgetMs) || reducerBudgetMs <= 0) throw new Error("reducer budget is invalid");
    if (!Number.isInteger(queueHighWaterMark) || queueHighWaterMark < maxItemsPerFrame) throw new Error("queue high water mark is invalid");
    this.store = store;
    this.workspaceId = workspaceId;
    this.sessionId = sessionId;
    this.agentRunId = agentRunId;
    this.lastCursor = initialCursor;
    this.receivedCursor = initialCursor;
    this.maxItemsPerFrame = maxItemsPerFrame;
    this.reducerBudgetMs = reducerBudgetMs;
    this.queueHighWaterMark = queueHighWaterMark;
    this.scheduleFrame = scheduleFrame;
    this.cancelFrame = cancelFrame;
    this.now = now;
    this.onAgentRunError = onAgentRunError;
    this.onCitationsChanged = onCitationsChanged;
    this.queue = [];
    this.queueOffset = 0;
    this.frameId = null;
    this.disposed = false;
    this.terminalAccepted = false;
    this.failure = null;
    this.idleWaiters = [];
    this.acceptedTelemetry = new WeakMap();
    this.acceptedOrdinal = 0;
  }

  accept(entry: StreamEntry) {
    if (this.disposed) throw new Error("chat controller is disposed");
    if (this.failure) throw this.failure;
    if (this.terminalAccepted) throw new Error("session stream emitted an item after terminal");
    if (entry.item.agentRunId !== this.agentRunId) throw new Error("Session stream AgentRun binding mismatch");
    if (entry.cursor !== null) {
      if (typeof entry.cursor !== "string" || !entry.cursor) throw new Error("session stream cursor is invalid");
      this.receivedCursor = entry.cursor;
    }
    this.terminalAccepted = entry.item.kind === "committed"
      && TERMINALS.has(entry.item.event.type);
    this.acceptedOrdinal += 1;
    this.acceptedTelemetry.set(entry, this.store.beginStreamTelemetry(entry, this.acceptedOrdinal));
    const previous = this.queue.length > this.queueOffset ? this.queue.at(-1) : undefined;
    if (canCoalesce(previous, entry)) this.queue[this.queue.length - 1] = entry;
    else this.queue.push(entry);
    this.ensureScheduled();
  }

  setCursor(cursor: string) {
    if (typeof cursor !== "string" || !cursor) throw new Error("resume cursor is invalid");
    if (this.hasQueuedItems()) throw new Error("cannot replace cursor while stream items are queued");
    this.lastCursor = cursor;
    this.receivedCursor = cursor;
  }

  async acceptWithBackpressure(entry: StreamEntry) {
    this.accept(entry);
    if (this.queuedItemCount() >= this.queueHighWaterMark) await this.whenIdle();
  }

  ensureScheduled() {
    if (this.frameId !== null || !this.hasQueuedItems() || this.disposed) return;
    this.frameId = this.scheduleFrame(() => this.drain());
  }

  hasQueuedItems() {
    return this.queueOffset < this.queue.length;
  }

  queuedItemCount() {
    return this.queue.length - this.queueOffset;
  }

  drain() {
    this.frameId = null;
    if (this.disposed) return;
    const startedAt = this.now();
    const batch: StreamEntry[] = [];
    while (this.hasQueuedItems() && batch.length < this.maxItemsPerFrame) {
      batch.push(this.queue[this.queueOffset++]);
      if (this.now() - startedAt >= this.reducerBudgetMs) break;
    }
    if (batch.length) {
      try {
        const telemetry = batch.map((entry) => this.acceptedTelemetry.get(entry));
        this.store.applyStreamEntries(batch, telemetry);
        const appliedCursor = [...batch].reverse().find((entry) => entry.cursor !== null)?.cursor;
        if (appliedCursor) this.lastCursor = appliedCursor;
        if (batch.some(({ item }) => item.kind === "committed"
          && (item.event.type === "tool_result" || item.event.type === "citation_recorded" || TERMINALS.has(item.event.type)))) {
          this.onCitationsChanged();
        }
        batch.forEach((entry) => this.acceptedTelemetry.delete(entry));
        const failed = [...batch].reverse().find((entry) => entry.item.kind === "committed" && entry.item.event.type === "agent_run_failed");
        if (failed?.item.kind === "committed") {
          this.onAgentRunError(failed.item.event.payload);
        }
      } catch (error) {
        this.failure = error instanceof Error ? error : new Error(String(error));
        this.queue = [];
        this.queueOffset = 0;
        this.rejectIdle(this.failure);
        return;
      }
    }
    if (this.hasQueuedItems()) this.ensureScheduled();
    else {
      this.queue = [];
      this.queueOffset = 0;
      this.resolveIdle();
    }
  }

  whenIdle() {
    if (this.failure) return Promise.reject(this.failure);
    if (!this.hasQueuedItems() && this.frameId === null) return Promise.resolve();
    return new Promise<void>((resolve, reject) => {
      this.idleWaiters.push({ resolve: () => resolve(), reject });
    });
  }

  resolveIdle() {
    this.idleWaiters.splice(0).forEach(({ resolve }) => resolve());
  }

  rejectIdle(error: Error) {
    this.idleWaiters.splice(0).forEach(({ reject }) => reject(error));
  }

  dispose() {
    this.disposed = true;
    this.queue = [];
    this.queueOffset = 0;
    this.acceptedTelemetry = new WeakMap();
    this.failure = null;
    if (this.frameId !== null) this.cancelFrame(this.frameId);
    this.frameId = null;
    this.resolveIdle();
  }
}
