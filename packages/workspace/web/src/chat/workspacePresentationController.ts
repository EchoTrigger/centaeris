import { validateTranscriptPatchPage, type TranscriptPage } from './transcriptContract.ts';
import type { TranscriptViewStore } from './transcriptViewStore.ts';
import { validatePresentationFrame, PresentationProtocolError, type PresentationIdentity, type PresentationEntry, type PresentationCheckpoint, type PresentationSession, type PresentationOperation, type PresentationViewRevision, type PresentationActivity, type PresentationQuestion, type PresentationLive, type PresentationLiveDelta } from './workspacePresentationProtocol.ts';

export type { PresentationActivity } from './workspacePresentationProtocol.ts';
const liveEncoder = new TextEncoder();
type LiveBaseline = Readonly<{ agentRunId: string; live: PresentationLive; textBytes: number; reasoningBytes: number }>;

type Options = PresentationIdentity & Readonly<{
  store: TranscriptViewStore;
  signal?: AbortSignal;
  isCurrent?(): boolean;
  applySnapshot?(page: TranscriptPage, signal: AbortSignal): number | Promise<number>;
  onViewRevision?(revision: PresentationViewRevision): void;
  onSnapshot?(page: TranscriptPage, epoch: number): void;
  onNavigation?(sessions: readonly PresentationSession[]): void;
  onOperation?(detail: PresentationOperation): void;
  onActivity?(activity: PresentationActivity): void;
  onQuestion?(question: PresentationQuestion): void;
  onCheckpoint?(checkpoint: PresentationCheckpoint): void;
  onApplied?(checkpoint: PresentationCheckpoint): void;
  onReset?(reason: 'cursorInvalid' | 'projectionInvalidated'): void;
  cursor?: string | null;
}>;

// Delivery is deliberately serial: the fetch reader awaits each application.
// There is no unbounded event queue and live snapshots cannot outrun the view.
export class WorkspacePresentationController {
  readonly identity: PresentationIdentity;
  readonly store: TranscriptViewStore;
  lastCursor: string | null = null;
  private viewEpoch: number;
  private acknowledgedSourceHighWater = "0";
  private readonly options: Options;
  private readonly signal: AbortSignal;
  private navigation: { snapshotId: string; rows: PresentationSession[]; ids: Set<string> } | null = null;
  private disposed = false;
  private applying = false;
  private viewRevision: PresentationViewRevision | null = null;
  private appliedCheckpoint: PresentationCheckpoint | null = null;
  private liveBaseline: LiveBaseline | null = null;
  constructor(options: Options) {
    if (!options.workspaceId) throw new Error('presentation identity is required');
    this.options = options;
    this.identity = { workspaceId: options.workspaceId, sessionId: options.sessionId };
    this.store = options.store;
    this.signal = options.signal ?? new AbortController().signal;
    this.viewEpoch = options.store.getListSnapshot().viewEpoch;
    this.lastCursor = options.cursor ?? null;
    this.acknowledgedSourceHighWater = options.store.getListSnapshot().appliedSourceHighWater;
  }
  private current() {
    return !this.disposed && !this.signal.aborted && (this.options.isCurrent?.() ?? true);
  }
  private applyLive(agentRunId: string, live: PresentationLive, textBytes: number, reasoningBytes: number) {
    const reasoning = live.reasoning ? Object.freeze({ ...live.reasoning }) : null;
    const next = Object.freeze({ ...live, reasoning });
    this.liveBaseline = { agentRunId, live: next, textBytes, reasoningBytes };
    this.store.applyPresentationLiveOverlay(Object.freeze({
      messageId: next.messageId, turnId: next.turnId,
      afterSourceHighWater: String(next.afterSequence), revision: next.revision,
      text: next.text, reasoning,
    }), this.viewEpoch);
  }
  private appendLive(agentRunId: string, delta: PresentationLiveDelta) {
    const baseline = this.liveBaseline;
    if (baseline === null || baseline.agentRunId !== agentRunId
      || baseline.live.afterSequence !== delta.afterSequence
      || baseline.live.turnId !== delta.turnId || baseline.live.messageId !== delta.messageId
      || baseline.live.revision !== delta.baseRevision || baseline.textBytes !== delta.textOffset) {
      // A lost baseline is recoverable through the existing bounded transport
      // reconnect; its next subscription starts with a complete live snapshot.
      throw new Error('presentation live delta baseline mismatch');
    }
    let reasoning = baseline.live.reasoning ?? null;
    let reasoningBytes = baseline.reasoningBytes;
    if (delta.reasoning !== null) {
      const change = delta.reasoning;
      if ((reasoning === null && change.offset !== 0) || (reasoning !== null
        && (reasoning.blockId !== change.blockId || reasoning.requestId !== change.requestId
          || reasoningBytes !== change.offset))) throw new Error('presentation live reasoning baseline mismatch');
      reasoning = { blockId: change.blockId, requestId: change.requestId, text: (reasoning?.text ?? '') + change.text };
      reasoningBytes += liveEncoder.encode(change.text).byteLength;
    }
    this.applyLive(agentRunId, {
      afterSequence: delta.afterSequence, turnId: delta.turnId, messageId: delta.messageId,
      revision: delta.revision, text: baseline.live.text + delta.text, reasoning,
    }, baseline.textBytes + liveEncoder.encode(delta.text).byteLength, reasoningBytes);
  }
  async acceptWithBackpressure(entry: PresentationEntry) {
    if (!this.current()) return;
    if (this.applying) throw new Error('presentation frames must be applied serially');
    this.applying = true;
    try {
      const item = validatePresentationFrame(entry.item, this.identity);
      if (entry.cursor !== null && item.kind !== 'checkpoint') throw new PresentationProtocolError('only checkpoints can carry a cursor');
      switch (item.kind) {
        case 'viewRevision': {
          const previous = this.viewRevision;
          const current = { ...item };
          for (const key of ['contextRevision', 'historyRevision', 'workRevision', 'subagentsRevision'] as const) {
            if (previous && BigInt(previous[key]) > BigInt(current[key])) current[key] = previous[key];
          }
          if (!previous || current.contextRevision !== previous.contextRevision || current.historyRevision !== previous.historyRevision || current.workRevision !== previous.workRevision || current.subagentsRevision !== previous.subagentsRevision) {
            this.viewRevision = current;
            this.options.onViewRevision?.(current);
          }
          break;
        }
        case 'navigation': {
          if (item.offset === 0) this.navigation = { snapshotId: item.snapshotId, rows: [], ids: new Set() };
          if (this.navigation === null || this.navigation.snapshotId !== item.snapshotId || this.navigation.rows.length !== item.offset) throw new PresentationProtocolError('navigation snapshot made no progress');
          for (const row of item.sessions) {
            if (this.navigation.ids.has(row.id)) throw new PresentationProtocolError('navigation snapshot duplicates a session');
            this.navigation.ids.add(row.id);
            this.navigation.rows.push(row);
          }
          if (item.hasMore && item.sessions.length === 0) throw new PresentationProtocolError('navigation snapshot made no progress');
          if (!item.hasMore) {
            this.options.onNavigation?.(this.navigation.rows);
            this.navigation = null;
          }
          break;
        }
        case 'operation': this.options.onOperation?.(item.detail); break;
        case 'activity': this.options.onActivity?.(item); break;
        case 'question': this.options.onQuestion?.(item); break;
        case 'reset': this.lastCursor = null; this.appliedCheckpoint = null; this.liveBaseline = null; this.options.onReset?.(item.reason); break;
        case 'snapshot': {
          const epoch = await this.store.withPresentationSnapshot(item.page, () => this.options.applySnapshot
            ? this.options.applySnapshot(item.page, this.signal)
            : this.store.openTail(item.page));
          if (!this.current()) return;
          this.viewEpoch = epoch;
          this.acknowledgedSourceHighWater = this.store.getListSnapshot().appliedSourceHighWater;
          if (!this.store.reservePresentationContent(epoch, item.page.blocks)) return;
          this.options.onSnapshot?.(item.page, epoch);
          break;
        }
        case 'patches': {
          const snapshot = this.store.getListSnapshot();
          if (snapshot.viewEpoch !== this.viewEpoch) return;
          if (snapshot.sessionId === null || snapshot.projectionVersion === null || snapshot.projectionGeneration === null) throw new PresentationProtocolError('patch requires an initial snapshot');
          const page = validateTranscriptPatchPage(item.page, {
            sessionId: snapshot.sessionId, projectionVersion: snapshot.projectionVersion,
            projectionGeneration: snapshot.projectionGeneration, sourceHighWater: snapshot.sourceHighWater,
            afterSourceHighWater: this.acknowledgedSourceHighWater,
          });
          this.store.preparePresentationContent(page.patches.flatMap(patch => [...patch.upserts]), this.viewEpoch);
          if (!this.store.applyPatchPage(page, this.viewEpoch, true, this.acknowledgedSourceHighWater)) return;
          this.store.reservePresentationContent(this.viewEpoch);
          break;
        }
        case 'content':
          if (!this.store.applyPresentationContentRange(item.range, this.viewEpoch)) return;
          break;
        case 'live':
          // A clear can precede its durable patch. Keep the displayed text until
          // the final applied checkpoint confirms its committed replacement.
          if (item.live === null) this.liveBaseline = null;
          else this.applyLive(item.agentRunId, item.live, liveEncoder.encode(item.live.text).byteLength,
            liveEncoder.encode(item.live.reasoning?.text ?? '').byteLength);
          break;
        case 'liveDelta':
          this.appendLive(item.agentRunId, item.delta);
          break;
        case 'checkpoint': {
          if (entry.cursor !== null && this.navigation !== null) throw new PresentationProtocolError('navigation snapshot is incomplete');
          this.options.onCheckpoint?.(item);
          if (entry.cursor === null) break;
          if (this.identity.sessionId !== null) {
            const snapshot = this.store.getListSnapshot();
            if (snapshot.viewEpoch !== this.viewEpoch) return;
            if (snapshot.sessionId !== item.sessionId || snapshot.projectionGeneration !== item.projectionGeneration
              || snapshot.appliedSourceHighWater !== item.sourceHighWater) throw new PresentationProtocolError('checkpoint projection is not applied');
            if (!this.store.presentationContentReady()) throw new PresentationProtocolError('checkpoint content is not applied');
            if (item.agentRun === null || !['queued', 'running'].includes(item.agentRun.status)) {
              this.store.clearLiveOverlay(this.viewEpoch);
              this.liveBaseline = null;
            }
          }
          this.lastCursor = entry.cursor;
          this.acknowledgedSourceHighWater = item.sourceHighWater;
          this.appliedCheckpoint = item;
          this.options.onApplied?.(item);
          break;
        }
      }
    } finally { this.applying = false; }
  }
  whenIdle() { return Promise.resolve(); }
  captureAppliedView() {
    const snapshot = this.store.getListSnapshot();
    if (!this.lastCursor || !this.appliedCheckpoint || this.navigation !== null
      || snapshot.sessionId !== this.identity.sessionId
      || snapshot.projectionGeneration !== this.appliedCheckpoint.projectionGeneration
      || snapshot.appliedSourceHighWater !== this.appliedCheckpoint.sourceHighWater
      || !this.store.presentationContentReady()) return null;
    return { cursor: this.lastCursor, restore: this.store.captureView() };
  }
  dispose() { this.disposed = true; }
}
