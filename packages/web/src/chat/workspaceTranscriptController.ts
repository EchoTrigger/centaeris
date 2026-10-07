import type { SessionStreamEvent, StreamEntry } from "./streamTypes.ts";
import type { TranscriptViewStore } from "./transcriptViewStore.ts";
import type { createWorkspaceTranscriptTransport } from "./transcriptTransport.ts";

const MAX_ITEMS_PER_FRAME = 128;
const QUEUE_HIGH_WATER_MARK = 512;
const TERMINAL_EVENT_TYPES = new Set([
  "agent_run_completed",
  "agent_run_failed",
  "agent_run_interrupted",
]);

type TranscriptTransport = Pick<ReturnType<typeof createWorkspaceTranscriptTransport>, "loadPatches">;

type IdleWaiter = Readonly<{
  resolve(): void;
  reject(reason: Error): void;
}>;

type ControllerOptions = Readonly<{
  store: TranscriptViewStore;
  viewEpoch: number;
  identity: Readonly<{
    sessionId: string;
    projectionVersion: string;
    projectionGeneration: string;
  }>;
  agentRunId: string;
  initialCursor: string;
  transport: TranscriptTransport;
  signal?: AbortSignal;
  scheduleFrame?: (callback: () => void | Promise<void>) => number;
  cancelFrame?: (frameId: number) => void;
  onCommittedEvent?: (event: SessionStreamEvent) => void;
  onDetailError?: (error: unknown, event: SessionStreamEvent) => void;
  onTerminal?: () => void;
}>;

function canCoalesce(left: StreamEntry | undefined, right: StreamEntry) {
  return left?.item.kind === "live"
    && right.item.kind === "live"
    && left.item.agentRunId === right.item.agentRunId
    && left.item.messageId === right.item.messageId;
}

export class WorkspaceTranscriptController {
  readonly store: TranscriptViewStore;
  readonly viewEpoch: number;
  readonly identity: ControllerOptions["identity"];
  readonly sessionId: string;
  readonly agentRunId: string;
  readonly transport: TranscriptTransport;
  readonly signal: AbortSignal;
  readonly scheduleFrame: NonNullable<ControllerOptions["scheduleFrame"]>;
  readonly cancelFrame: NonNullable<ControllerOptions["cancelFrame"]>;
  readonly onCommittedEvent: NonNullable<ControllerOptions["onCommittedEvent"]>;
  readonly onDetailError: NonNullable<ControllerOptions["onDetailError"]>;
  readonly onTerminal: NonNullable<ControllerOptions["onTerminal"]>;
  lastCursor: string;
  receivedCursor: string;
  private queue: StreamEntry[] = [];
  private queueOffset = 0;
  private frameId: number | null = null;
  private draining = false;
  private disposed = false;
  private terminalAccepted = false;
  private failure: Error | null = null;
  private failureAbort = new AbortController();
  get failureSignal() { return this.failureAbort.signal; }
  private idleWaiters: IdleWaiter[] = [];

  constructor({
    store,
    viewEpoch,
    identity,
    agentRunId,
    initialCursor,
    transport,
    signal = new AbortController().signal,
    scheduleFrame = (callback) => requestAnimationFrame(() => { void callback(); }),
    cancelFrame = (frameId) => cancelAnimationFrame(frameId),
    onCommittedEvent = () => {},
    onDetailError = (error, event) => console.error("Transcript tool details unavailable", {
      eventId: event.eventId, agentRunId: event.agentRunId, error,
    }),
    onTerminal = () => {},
  }: ControllerOptions) {
    if (!agentRunId || !initialCursor || !identity.sessionId
      || !identity.projectionVersion || !identity.projectionGeneration) {
      throw new Error("transcript controller identity is required");
    }
    this.store = store;
    this.viewEpoch = viewEpoch;
    this.identity = identity;
    this.sessionId = identity.sessionId;
    this.agentRunId = agentRunId;
    this.lastCursor = initialCursor;
    this.receivedCursor = initialCursor;
    this.transport = transport;
    this.signal = signal;
    this.scheduleFrame = scheduleFrame;
    this.cancelFrame = cancelFrame;
    this.onCommittedEvent = onCommittedEvent;
    this.onDetailError = onDetailError;
    this.onTerminal = onTerminal;
  }

  accept(entry: StreamEntry) {
    if (this.disposed) throw new Error("transcript controller is disposed");
    if (this.failure) throw this.failure;
    if (this.terminalAccepted) throw new Error("session stream emitted an item after terminal");
    if (entry.item.agentRunId !== this.agentRunId) {
      throw new Error("Session stream AgentRun binding mismatch");
    }
    if (entry.cursor !== null) this.receivedCursor = entry.cursor;
    this.terminalAccepted = entry.item.kind === "committed"
      && TERMINAL_EVENT_TYPES.has(entry.item.event.type);
    const previous = this.queue.length > this.queueOffset ? this.queue.at(-1) : undefined;
    if (canCoalesce(previous, entry)) this.queue[this.queue.length - 1] = entry;
    else this.queue.push(entry);
    this.ensureScheduled();
  }

  async acceptWithBackpressure(entry: StreamEntry) {
    this.accept(entry);
    if (this.queue.length - this.queueOffset >= QUEUE_HIGH_WATER_MARK) await this.whenIdle();
  }

  setCursor(cursor: string) {
    if (!cursor || this.queueOffset < this.queue.length || this.draining) {
      throw new Error("cannot replace transcript stream cursor while work is queued");
    }
    this.lastCursor = cursor;
    this.receivedCursor = cursor;
  }

  private ensureScheduled() {
    if (this.frameId !== null || this.draining || this.disposed || this.queueOffset >= this.queue.length) return;
    this.frameId = this.scheduleFrame(() => this.drain());
  }

  private async drain() {
    this.frameId = null;
    if (this.disposed || this.draining) return;
    this.draining = true;
    const batch = this.queue.slice(this.queueOffset, this.queueOffset + MAX_ITEMS_PER_FRAME);
    this.queueOffset += batch.length;
    try {
      if (batch.some((entry) => entry.item.kind === "committed")) {
        for (const entry of batch) {
          if (entry.item.kind === "committed") {
            try { this.onCommittedEvent(entry.item.event); }
            catch (error) { this.onDetailError(error, entry.item.event); }
          }
        }
        const afterSourceHighWater = this.store.getListSnapshot().appliedSourceHighWater;
        await this.transport.loadPatches({
          ...this.identity,
          afterSourceHighWater,
        }, this.signal, (page) => {
          if (!this.store.applyPatchPage(page, this.viewEpoch)) {
            throw new Error("transcript view epoch changed while applying patches");
          }
        });
      }
      for (const entry of batch) {
        if (entry.item.kind !== "live") continue;
        this.store.applyLiveOverlay({
          messageId: entry.item.messageId,
          turnId: entry.item.turnId,
          afterSourceHighWater: String(entry.item.afterSequence),
          revision: entry.item.revision,
          text: entry.item.text,
          reasoning: entry.item.reasoning || null,
        }, this.viewEpoch);
      }
      const terminal = batch.some((entry) => entry.item.kind === "committed"
        && TERMINAL_EVENT_TYPES.has(entry.item.event.type));
      if (terminal) {
        this.store.clearLiveOverlay(this.viewEpoch);
        this.onTerminal();
      }
      const appliedCursor = [...batch].reverse().find((entry) => entry.cursor !== null)?.cursor;
      if (appliedCursor) this.lastCursor = appliedCursor;
    } catch (error) {
      this.failure = error instanceof Error ? error : new Error(String(error));
      this.failureAbort.abort(this.failure);
      this.queue = [];
      this.queueOffset = 0;
      this.rejectIdle(this.failure);
      return;
    } finally {
      this.draining = false;
    }
    if (this.queueOffset < this.queue.length) this.ensureScheduled();
    else {
      this.queue = [];
      this.queueOffset = 0;
      this.resolveIdle();
    }
  }

  whenIdle() {
    if (this.failure) return Promise.reject(this.failure);
    if (!this.draining && this.frameId === null && this.queueOffset >= this.queue.length) {
      return Promise.resolve();
    }
    return new Promise<void>((resolve, reject) => {
      this.idleWaiters.push({ resolve, reject });
    });
  }

  private resolveIdle() {
    this.idleWaiters.splice(0).forEach(({ resolve }) => resolve());
  }

  private rejectIdle(error: Error) {
    this.idleWaiters.splice(0).forEach(({ reject }) => reject(error));
  }

  dispose() {
    this.disposed = true;
    this.queue = [];
    this.queueOffset = 0;
    if (this.frameId !== null) this.cancelFrame(this.frameId);
    this.frameId = null;
    this.failure = null;
    this.resolveIdle();
  }
}
