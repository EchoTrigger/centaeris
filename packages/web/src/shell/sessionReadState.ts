import { sessionAttentionSequence as sequence, validateSessionAttention as validate, type SessionAttention } from "../sessionAttentionContract.ts";
export type { SessionAttention } from "../sessionAttentionContract.ts";

export function mergeSessionAttention<T extends SessionAttention>(current: T, incoming: T): T {
  validate(current); validate(incoming);
  if (current.id !== incoming.id || current.workspaceId !== incoming.workspaceId || current.agentId !== incoming.agentId) {
    throw new Error("session attention identity mismatch");
  }
  if (incoming.attentionRevision === current.attentionRevision
    && (incoming.latestAttentionSequence !== current.latestAttentionSequence
      || incoming.readThroughSequence !== current.readThroughSequence
      || incoming.isManuallyUnread !== current.isManuallyUnread)) {
    throw new Error("session attention revision conflict");
  }
  const older = sequence(incoming.attentionRevision) < sequence(current.attentionRevision);
  const state = older ? current : incoming;
  if (!older && (sequence(incoming.latestAttentionSequence) < sequence(current.latestAttentionSequence)
    || sequence(incoming.readThroughSequence) < sequence(current.readThroughSequence))) {
    throw new Error("session attention watermarks regressed");
  }
  return { ...incoming,
    latestAttentionSequence: state.latestAttentionSequence,
    readThroughSequence: state.readThroughSequence,
    attentionRevision: state.attentionRevision,
    isManuallyUnread: state.isManuallyUnread,
    isUnread: state.isManuallyUnread || sequence(state.latestAttentionSequence) > sequence(state.readThroughSequence),
  };
}

export function mergeSessionNavigation<T extends SessionAttention>(current: readonly T[], incoming: readonly T[]): T[] {
  const byId = new Map(current.map(record => [record.id, record]));
  return incoming.map(record => {
    validate(record);
    const previous = byId.get(record.id);
    return previous ? mergeSessionAttention(previous, record) : record;
  });
}

type Observation = Readonly<{
  session: SessionAttention | null;
  visible: boolean;
  readingLatest: boolean;
  appliedSourceHighWater: string;
}>;

type Options = Readonly<{
  send(session: SessionAttention, throughSequence: string, signal: AbortSignal): Promise<SessionAttention>;
  onAcknowledged(session: SessionAttention): void;
  onError?(error: unknown): void;
}>;

/** Read acknowledgements are user observations, separate from delivery cursors.
 * Coalesce while a request is in flight; a failed write waits for a new observation. */
export class SessionReadController {
  private readonly options: Options;
  private readonly acknowledged = new Map<string, bigint>();
  private desired: Observation | null = null;
  private busy = false;
  private disposed = false;
  private readonly abort = new AbortController();

  constructor(options: Options) { this.options = options; }

  observe(observation: Observation) {
    if (this.disposed) return;
    try {
      if (typeof observation.visible !== "boolean" || typeof observation.readingLatest !== "boolean") throw new Error("session read observation invalid");
      sequence(observation.appliedSourceHighWater);
      if (observation.session !== null) validate(observation.session);
      this.desired = { ...observation, session: observation.session === null ? null : { ...observation.session } };
    } catch (error) {
      this.desired = null;
      this.options.onError?.(error);
      return;
    }
    this.scheduleFlush();
  }

  private target() {
    const observed = this.desired;
    if (!observed?.session || !observed.visible || !observed.readingLatest || observed.session.isManuallyUnread) return null;
    const record = observed.session;
    validate(record);
    const through = sequence(record.latestAttentionSequence);
    const confirmed = this.acknowledged.get(this.key(record)) ?? 0n;
    if (through <= confirmed || through <= sequence(record.readThroughSequence)
      || through > sequence(observed.appliedSourceHighWater)) return null;
    return { record, through };
  }

  private key(record: SessionAttention) { return JSON.stringify([record.workspaceId, record.agentId, record.id]); }

  private scheduleFlush() {
    // observe() owns the spawned promise. Even an observer callback failure is
    // reported instead of escaping as an unhandled asynchronous rejection.
    void this.flush().catch(error => { if (!this.disposed) console.error("Session read observer failed", error); });
  }

  private async flush() {
    if (this.busy || this.disposed) return;
    const target = this.target();
    if (!target) return;
    this.busy = true;
    let succeeded = false;
    try {
      const result = await this.options.send(target.record, target.through.toString(), this.abort.signal);
      if (this.disposed) return;
      validate(result);
      if (result.id !== target.record.id || result.workspaceId !== target.record.workspaceId
        || result.agentId !== target.record.agentId || sequence(result.readThroughSequence) < target.through) {
        throw new Error("session read acknowledgement identity invalid");
      }
      mergeSessionAttention(target.record, result);
      this.options.onAcknowledged(result);
      this.acknowledged.set(this.key(result), sequence(result.readThroughSequence));
      succeeded = true;
    } catch (error) {
      if (!this.disposed) this.options.onError?.(error);
    } finally {
      this.busy = false;
      if (!this.disposed) {
        const next = this.target();
        if (next && (succeeded || this.key(next.record) !== this.key(target.record) || next.through > target.through)) this.scheduleFlush();
      }
    }
  }

  dispose() { this.disposed = true; this.desired = null; this.abort.abort(); }
}