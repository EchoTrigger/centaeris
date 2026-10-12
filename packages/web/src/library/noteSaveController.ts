import type { NoteDocument, NoteDraft, NoteDraftStore, NoteSaveOperation } from "./noteDraftStore.ts";

export type NoteSaveResult = {
  saveId: string; appliedGeneration: number;
  object: { id: string; contentGeneration: number; displayName?: string; [key: string]: unknown };
};
export type NoteSaveStatus = "unsaved" | "local" | "syncing" | "saved" | "error" | "localError" | "conflict";
export type NoteSaveState = { status: NoteSaveStatus; draft: NoteDraft; error: string };
type Clock = { now(): number; setTimeout(fn: () => void, delay: number): number; clearTimeout(timer: number | undefined): void };
type Options = {
  userId: string; noteId: string | null; clientDraftId: string; editInstanceId: string;
  parentFolderId: string; baseGeneration: number; document: NoteDocument; recovered?: NoteDraft;
  store: NoteDraftStore; send(operation: NoteSaveOperation): Promise<NoteSaveResult>;
  onState(state: NoteSaveState): void; onConfirmed?(result: NoteSaveResult, document: NoteDocument): void; clock?: Clock;
};
type DraftWrite = { snapshot: NoteDraft; waiters: { resolve(): void; reject(error: unknown): void }[] };
const same = (a: NoteDocument, b: NoteDocument) => a.displayName === b.displayName && a.markdown === b.markdown;
const operationId = () => crypto.randomUUID().replaceAll("-", "");

export class NoteSaveController {
  private options: Options;
  private clock: Clock;
  private record: NoteDraft;
  private acknowledged: NoteDocument;
  private status: NoteSaveStatus = "saved";
  private error = "";
  private active = true;
  private timer: number | undefined;
  private localTimer: number | undefined;
  private dirtySince: number | null = null;
  private lastStarted = -Infinity;
  private running: Promise<void> | null = null;
  private writes: Promise<void> = Promise.resolve();
  private durableSequence = -1;
  private queuedWrite: DraftWrite | null = null;
  private writing = false;

  constructor(options: Options) {
    this.options = options;
    this.clock = options.clock || { now: () => Date.now(), setTimeout: (fn, delay) => window.setTimeout(fn, delay), clearTimeout: id => window.clearTimeout(id) };
    this.acknowledged = structuredClone(options.document);
    this.record = options.recovered ? structuredClone(options.recovered) : {
      schema: 1, key: "", userId: options.userId, noteId: options.noteId, clientDraftId: options.clientDraftId,
      editInstanceId: options.editInstanceId, parentFolderId: options.parentFolderId,
      baseGeneration: options.baseGeneration, sequence: 0, document: structuredClone(options.document), pending: null, updatedAt: this.clock.now(),
    };
    if (this.record.userId !== options.userId || (options.noteId && this.record.noteId !== options.noteId)) throw new Error("note_draft_identity_mismatch");
    this.record.editInstanceId = options.editInstanceId;
    this.record.key = JSON.stringify([options.userId, this.record.clientDraftId, options.editInstanceId]);
    if (options.recovered) { this.status = "local"; this.durableSequence = this.record.sequence; }
    else if (this.changed()) {
      this.status = "unsaved";
      this.dirtySince = this.clock.now();
      this.schedule();
    }
  }

  get state(): NoteSaveState { return { status: this.status, draft: structuredClone(this.record), error: this.error }; }
  get requiresDraftPersistence() { return this.changed() && this.durableSequence !== this.record.sequence; }
  private emit(status: NoteSaveStatus, error = "") {
    this.status = status; this.error = error;
    if (this.active) this.options.onState(this.state);
  }
  private changed() {
    return Boolean(this.record.pending) || (!this.record.noteId && (Boolean(this.record.document.markdown.trim())
      || this.record.document.displayName !== "Untitled")) || !same(this.record.document, this.acknowledged);
  }
  private localStatus(): "local" | "unsaved" { return this.durableSequence === this.record.sequence ? "local" : "unsaved"; }

  edit(document: NoteDocument) {
    if (!this.active || same(this.record.document, document)) return;
    this.record.document = structuredClone(document);
    this.record.sequence++;
    this.record.updatedAt = this.clock.now();
    this.dirtySince ??= this.clock.now();
    if (!["conflict", "error", "localError"].includes(this.status)) this.emit("unsaved");
    if (this.localTimer === undefined) this.localTimer = this.clock.setTimeout(() => {
      this.localTimer = undefined;
      void this.persist().catch(() => {});
    }, 100);
    this.schedule();
  }

  async persist() {
    const snapshot = structuredClone(this.record);
    const operation = new Promise<void>((resolve, reject) => {
      if (this.queuedWrite) { this.queuedWrite.snapshot = snapshot; this.queuedWrite.waiters.push({ resolve, reject }); }
      else this.queuedWrite = { snapshot, waiters: [{ resolve, reject }] };
      this.startWrites();
    });
    try {
      await operation;
      if (snapshot.sequence === this.record.sequence && ["unsaved", "local", "localError"].includes(this.status)) this.emit("local");
    } catch (error) {
      this.emit("localError", error instanceof Error ? error.message : "note_draft_write_failed");
      throw error;
    }
  }

  private startWrites() {
    if (this.writing || !this.queuedWrite) return;
    this.writing = true;
    this.writes = this.drainWrites().finally(() => { this.writing = false; this.startWrites(); });
  }

  private async drainWrites() {
    while (this.queuedWrite) {
      const batch = this.queuedWrite;
      this.queuedWrite = null;
      try {
        await this.options.store.put(batch.snapshot);
        this.durableSequence = batch.snapshot.sequence;
        for (const waiter of batch.waiters) waiter.resolve();
      } catch (error) {
        for (const waiter of batch.waiters) waiter.reject(error);
      }
    }
  }

  private schedule() {
    this.clock.clearTimeout(this.timer);
    if (!this.active || this.running || !this.changed() || ["conflict", "error", "localError"].includes(this.status)) return;
    const now = this.clock.now();
    const due = Math.max(Math.min(now + 2000, (this.dirtySince ?? now) + 10000), this.lastStarted + 5000);
    this.timer = this.clock.setTimeout(() => { void this.flush(); }, Math.max(0, due - now));
  }

  flush(): Promise<void> {
    this.clock.clearTimeout(this.timer);
    if (this.running) return this.running.then(() => {
      if (this.active && this.changed() && !["conflict", "error", "localError"].includes(this.status)) return this.flush();
    });
    if (!this.active || this.status === "conflict") return Promise.resolve();
    this.running = this.perform().finally(() => { this.running = null; this.schedule(); });
    return this.running;
  }

  private async cleanConfirmed(sequence = this.record.sequence) {
    await this.writes;
    await this.options.store.deleteIfSequence(this.record.key, sequence);
    if (this.options.recovered) await this.options.store.deleteIfSequence(this.options.recovered.key, this.options.recovered.sequence);
    if (this.record.sequence === sequence) this.dirtySince = null;
  }

  private async perform() {
    if (!this.changed()) {
      try { await this.cleanConfirmed(); this.emit(this.changed() ? this.localStatus() : "saved"); }
      catch (error) { this.emit("localError", error instanceof Error ? error.message : "note_draft_cleanup_failed"); }
      return;
    }
    try {
      this.clock.clearTimeout(this.localTimer); this.localTimer = undefined;
      if (!this.record.pending) this.record.pending = {
        saveId: operationId(), sequence: this.record.sequence, noteId: this.record.noteId,
        baseGeneration: this.record.baseGeneration, document: structuredClone(this.record.document), parentFolderId: this.record.parentFolderId,
      };
      // Persist operation identity and exact request before any network side effect.
      await this.persist();
    } catch { return; }
    const operation = structuredClone(this.record.pending!);
    this.lastStarted = this.clock.now();
    this.emit("syncing");
    try {
      const result = await this.options.send(operation);
      if (result.saveId !== operation.saveId || !result.object?.id
          || !Number.isSafeInteger(result.appliedGeneration) || result.appliedGeneration < operation.baseGeneration
          || result.object.contentGeneration !== result.appliedGeneration
          || (operation.noteId && result.object.id !== operation.noteId)) throw new Error("note_save_response_invalid");
      this.record.noteId = result.object.id;
      this.record.pending = null;
      if (this.options.baseGeneration > result.appliedGeneration) {
        // A historical receipt resolves this operation, not the newer head we opened.
        this.record.baseGeneration = this.options.baseGeneration;
        await this.persist();
        this.emit("conflict", "note_generation_conflict");
        return;
      }
      this.record.baseGeneration = result.appliedGeneration;
      this.acknowledged = structuredClone(operation.document);
      await this.persist();
      if (this.record.sequence === operation.sequence && same(this.record.document, operation.document)) {
        await this.cleanConfirmed(operation.sequence);
        this.emit(this.changed() ? this.localStatus() : "saved");
      } else this.emit(this.localStatus());
      if (this.active) this.options.onConfirmed?.(result, structuredClone(operation.document));
    } catch (error) {
      if (this.status === "localError") return;
      const message = error instanceof Error ? error.message : "note_save_failed";
      if (["note_generation_conflict", "library_name_conflict"].includes(message)
          || (error as { status?: number })?.status === 400 || (error as { status?: number })?.status === 413) {
        this.record.pending = null;
        try { await this.persist(); } catch { return; }
      }
      this.emit(message === "note_generation_conflict" ? "conflict" : "error", message);
    }
  }

  rebase(generation: number) {
    if (this.status !== "conflict" || !Number.isSafeInteger(generation) || generation < 1) throw new Error("note_rebase_invalid");
    this.record.baseGeneration = generation;
    // Explicit replacement is a new intent even if this body succeeded in an older generation.
    this.record.pending = {
      saveId: operationId(), sequence: this.record.sequence, noteId: this.record.noteId,
      baseGeneration: generation, document: structuredClone(this.record.document), parentFolderId: this.record.parentFolderId,
    };
    this.emit("unsaved");
  }

  dispose() {
    this.active = false;
    this.clock.clearTimeout(this.timer);
    this.clock.clearTimeout(this.localTimer);
    this.localTimer = undefined;
    if (this.changed()) void this.persist().catch(() => {});
    return this.running || Promise.resolve();
  }
}
