import { validateTranscriptPatchPage, type TranscriptPage } from './transcriptContract.ts';
import type { TranscriptViewStore } from './transcriptViewStore.ts';
import { validatePresentationFrame, PresentationProtocolError, type PresentationIdentity, type PresentationEntry, type PresentationCheckpoint, type PresentationSession, type PresentationOperation } from './workspacePresentationProtocol.ts';

type Options = PresentationIdentity & Readonly<{
  store: TranscriptViewStore;
  signal?: AbortSignal;
  isCurrent?(): boolean;
  applySnapshot?(page: TranscriptPage, signal: AbortSignal): number | Promise<number>;
  onSnapshot?(page: TranscriptPage, epoch: number): void;
  onNavigation?(sessions: readonly PresentationSession[]): void;
  onOperation?(detail: PresentationOperation): void;
  onCheckpoint?(checkpoint: PresentationCheckpoint): void;
  onApplied?(checkpoint: PresentationCheckpoint): void;
  onReset?(reason: 'cursorInvalid' | 'projectionInvalidated'): void;
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
  constructor(options: Options) {
    if (!options.workspaceId || !options.agentId) throw new Error('presentation identity is required');
    this.options = options;
    this.identity = { workspaceId: options.workspaceId, agentId: options.agentId, sessionId: options.sessionId };
    this.store = options.store;
    this.signal = options.signal ?? new AbortController().signal;
    this.viewEpoch = options.store.getListSnapshot().viewEpoch;
  }
  private current() {
    return !this.disposed && !this.signal.aborted && (this.options.isCurrent?.() ?? true);
  }
  async acceptWithBackpressure(entry: PresentationEntry) {
    if (!this.current()) return;
    if (this.applying) throw new Error('presentation frames must be applied serially');
    this.applying = true;
    try {
      const item = validatePresentationFrame(entry.item, this.identity);
      if (entry.cursor !== null && item.kind !== 'checkpoint') throw new PresentationProtocolError('only checkpoints can carry a cursor');
      switch (item.kind) {
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
        case 'reset': this.lastCursor = null; this.options.onReset?.(item.reason); break;
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
          if (item.live !== null) this.store.applyLiveOverlay({
            messageId: item.live.messageId, turnId: item.live.turnId,
            afterSourceHighWater: String(item.live.afterSequence), revision: item.live.revision,
            text: item.live.text, reasoning: item.live.reasoning ?? null,
          }, this.viewEpoch);
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
            if (item.agentRun === null || !['queued', 'running'].includes(item.agentRun.status)) this.store.clearLiveOverlay(this.viewEpoch);
          }
          this.lastCursor = entry.cursor;
          this.acknowledgedSourceHighWater = item.sourceHighWater;
          this.options.onApplied?.(item);
          break;
        }
      }
    } finally { this.applying = false; }
  }
  whenIdle() { return Promise.resolve(); }
  dispose() { this.disposed = true; }
}
