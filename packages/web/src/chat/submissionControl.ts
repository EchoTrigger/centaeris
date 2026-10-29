import { ApiError } from "../api.ts";
import type { OperationClient, OperationReceipt, OperationSubmissionHooks } from "./operationReceipts.ts";

export type SubmissionTarget = { sessionId: string; agentRunId: string };
type TerminalStatus = "completed" | "failed" | "cancelled";
type Terminal = { status: TerminalStatus; completedAtMs?: number };
export type SubmissionPhase = "idle" | "preparing" | "submitting" | "accepted" | "recovering" | "cancelling" | "stopRequested" | "unknown" | "rejected" | "terminal";
export type SubmissionSnapshot = Readonly<{
  attemptId: number;
  phase: SubmissionPhase;
  operationId: string | null;
  target: SubmissionTarget | null;
  stopRequested: boolean;
  feedback: "stopRecorded" | "stopInProgress" | "stopRequested" | "stopUnconfirmed" | "stoppedBeforeDispatch" | "terminal" | "pendingStop" | "stopRejected" | "submissionRejected" | null;
  error: unknown;
  terminal: Terminal | null;
}>;
export type SubmissionCancellation = { agentRunId: string; status: string; disposition: string };
type Storage = Pick<globalThis.Storage, "getItem" | "setItem" | "removeItem">;
type Stored = { schema: "web.submission_control.v1"; operationId: string | null; target: SubmissionTarget | null; stopRequested: true; terminal: Terminal | null };
const isExplicitRejection = (error: unknown) => error instanceof ApiError && [400, 401, 403, 404, 410, 422, 429].includes(error.status)
  && !(error.status === 404 && error.message === "operation_not_found");
const keyOf = (target: SubmissionTarget) => JSON.stringify([target.sessionId, target.agentRunId]);
const sameTarget = (left: SubmissionTarget | null, right: SubmissionTarget | null) => Boolean(left && right && keyOf(left) === keyOf(right));
const exactKeys = (value: object, keys: string[]) => Object.keys(value).sort().join("|") === keys.sort().join("|");
const nonempty = (value: unknown): value is string => typeof value === "string" && value.trim().length > 0;
const isTarget = (value: unknown): value is SubmissionTarget => Boolean(value && typeof value === "object"
  && exactKeys(value, ["sessionId", "agentRunId"]) && nonempty((value as SubmissionTarget).sessionId) && nonempty((value as SubmissionTarget).agentRunId));
const isTerminal = (value: unknown): value is Terminal => Boolean(value && typeof value === "object"
  && exactKeys(value, (value as Terminal).completedAtMs === undefined ? ["status"] : ["status", "completedAtMs"])
  && ["completed", "failed", "cancelled"].includes((value as Terminal).status)
  && ((value as Terminal).completedAtMs === undefined || Number.isFinite((value as Terminal).completedAtMs)));

function parseStored(stored: string): Stored {
  try {
    const value = JSON.parse(stored) as Stored;
    if (!value || typeof value !== "object" || !exactKeys(value, ["schema", "operationId", "target", "stopRequested", "terminal"])
      || value.schema !== "web.submission_control.v1" || value.stopRequested !== true
      || (value.operationId !== null && (typeof value.operationId !== "string" || !/^[A-Za-z0-9_.:-]{1,128}$/.test(value.operationId)))
      || (value.target !== null && !isTarget(value.target)) || (!value.operationId && !value.target)
      || (value.terminal !== null && (!isTerminal(value.terminal) || !value.target))) throw Error();
    return value;
  } catch { throw new Error("submission_control_storage_invalid"); }
}

/** Keeps a user's stop attached to the original command, independently of the
 * currently displayed conversation. No input text, file bytes or credentials
 * are retained. A transport timeout never proves that dispatch was rejected. */
export class SubmissionControl {
  private operations: Pick<OperationClient, "pending" | "recover"> & Partial<Pick<OperationClient, "complete">>;
  private storage: Storage;
  private storageKey: string;
  private scopeIdentity: string[];
  private submissionEpoch = 0;
  private preparationStop: ((error: Error) => void) | null = null;
  private hadPendingSubmission = false;
  private dispatched = false;
  private retainCancellation = false;
  private cancel: (target: SubmissionTarget) => Promise<SubmissionCancellation>;
  private listeners = new Set<() => void>();
  private snapshot: SubmissionSnapshot;
  private submitPromise: Promise<OperationReceipt> | null = null;
  private actionPromise: Promise<SubmissionSnapshot> | null = null;
  private terminals = new Map<string, Terminal>();

  constructor(options: {
    operations: Pick<OperationClient, "pending" | "recover"> & Partial<Pick<OperationClient, "complete">>;
    scope: { userId: string; workspaceId: string; controlId?: string };
    storage?: Storage;
    cancel: (target: SubmissionTarget) => Promise<SubmissionCancellation>;
  }) {
    this.operations = options.operations;
    this.storage = options.storage ?? sessionStorage;
    this.cancel = options.cancel;
    const identity = [options.scope.userId, options.scope.workspaceId];
    this.scopeIdentity = [...identity];
    if (options.scope.controlId !== undefined) identity.push(options.scope.controlId);
    this.storageKey = `centaeris.submissionControl.v1:${JSON.stringify(identity)}`;
    const pending = this.operations.pending();
    this.snapshot = Object.freeze({ attemptId: 0, phase: pending ? "unknown" : "idle", operationId: pending?.operationId ?? null,
      target: null, stopRequested: false, feedback: null, error: null, terminal: null });
    const stored = this.storage.getItem(this.storageKey);
    if (stored !== null) {
      const value = parseStored(stored);
      this.snapshot = Object.freeze({ ...this.snapshot, operationId: value.operationId, target: value.target, stopRequested: true, terminal: value.terminal,
        attemptId: 1, phase: value.terminal ? "terminal" : "unknown",
        feedback: value.terminal ? "terminal" : "stopUnconfirmed" });
      if (value.target && value.terminal) this.terminals.set(keyOf(value.target), value.terminal);
      this.retainCancellation = Boolean(value.target && !value.terminal);
    }
  }

  getSnapshot = () => this.snapshot;
  subscribe = (listener: () => void) => { this.listeners.add(listener); return () => { this.listeners.delete(listener); }; };

  private publish(changes: Partial<SubmissionSnapshot>) {
    this.snapshot = Object.freeze({ ...this.snapshot, ...changes });
    for (const listener of this.listeners) listener();
  }

  private persistStop() {
    const { operationId, target, stopRequested, terminal } = this.snapshot;
    if (!stopRequested || (!operationId && !target)) return;
    const value: Stored = { schema: "web.submission_control.v1", operationId, target, stopRequested: true, terminal };
    this.storage.setItem(this.storageKey, JSON.stringify(value));
  }

  begin() {
    if (this.snapshot.stopRequested && this.snapshot.phase !== "terminal") {
      if (this.snapshot.phase === "rejected" && this.snapshot.target && !this.actionPromise) this.preserveRejectedStop();
      else { this.publish({ feedback: "pendingStop" }); throw new Error("submission_stop_pending"); }
    }
    if ((this.submitPromise || this.actionPromise) && this.snapshot.phase !== "terminal") throw new Error("submission_pending");
    const pending = this.operations.pending();
    this.storage.removeItem(this.storageKey);
    this.submitPromise = null; this.actionPromise = null; this.preparationStop = null;
    this.submissionEpoch += 1; this.hadPendingSubmission = Boolean(pending); this.dispatched = Boolean(pending); this.retainCancellation = false;
    this.publish({ attemptId: this.snapshot.attemptId + 1, phase: "preparing", operationId: pending?.operationId ?? null,
      target: null, stopRequested: false, feedback: null, error: null, terminal: null });
    return this.snapshot.attemptId;
  }

  submit(work: (hooks: OperationSubmissionHooks) => Promise<OperationReceipt>): Promise<OperationReceipt> {
    if (this.submitPromise) return this.submitPromise;
    if (this.snapshot.phase !== "preparing") this.begin();
    const attempt = this.snapshot.attemptId;
    const epoch = this.submissionEpoch;
    const owns = () => this.snapshot.attemptId === attempt && this.submissionEpoch === epoch;
    const hooks: OperationSubmissionHooks = {
      shouldDispatch: () => owns() && !this.snapshot.stopRequested && this.snapshot.phase !== "terminal",
      onOperationId: operationId => {
        if (!owns()) throw new Error("operation_result_stale");
        if (this.snapshot.operationId && this.snapshot.operationId !== operationId) throw new Error("operation_result_stale");
        this.publish({ operationId }); this.persistStop();
      },
      onDispatch: () => {
        if (!owns()) return;
        this.publish({ phase: "submitting" });
        if (owns()) this.dispatched = true;
      },
    };
    const stopped = new Promise<OperationReceipt>((_resolve, reject) => { this.preparationStop = reject; });
    const execution = Promise.resolve().then(() => work(hooks));
    const pending = Promise.race([execution, stopped]).then(receipt => {
      if (!owns()) return receipt;
      if (this.snapshot.operationId && receipt.operationId !== this.snapshot.operationId) throw new Error("operation_result_stale");
      const target = { sessionId: receipt.sessionId, agentRunId: receipt.agentRunId! };
      if (!isTarget(target)) throw new Error("operation_receipt_invalid");
      const terminal = this.terminals.get(keyOf(target)) ?? null;
      this.publish({ operationId: receipt.operationId, target, terminal, phase: terminal ? "terminal" : "accepted",
        feedback: terminal ? "terminal" : this.snapshot.stopRequested ? "stopRecorded" : null, error: null });
      this.persistStop();
      if (this.snapshot.stopRequested && !terminal) void this.cancelTarget();
      return receipt;
    }).catch(error => {
      if (owns() && this.snapshot.phase !== "terminal") {
        const undispatched = error?.message === "operation_dispatch_stopped" && !this.operations.pending();
        const rejected = error instanceof ApiError && [400, 422, 429].includes(error.status) && !this.operations.pending();
        if (undispatched || rejected) {
          this.storage.removeItem(this.storageKey);
          this.publish({ phase: "idle", operationId: null, stopRequested: false, feedback: undispatched ? "stoppedBeforeDispatch" : null, error: rejected ? error : null });
        } else if (isExplicitRejection(error)) {
          this.rejectStop(error, false);
        } else {
          this.publish({ phase: this.operations.pending() || this.snapshot.stopRequested ? "unknown" : "idle", error,
            feedback: this.snapshot.stopRequested ? "stopUnconfirmed" : null });
          this.persistStop();
        }
      }
      throw error;
    }).finally(() => { if (this.submitPromise === pending) { this.submitPromise = null; this.preparationStop = null; } });
    this.submitPromise = pending;
    return pending;
  }

  stop(): Promise<SubmissionSnapshot> {
    if (this.snapshot.phase === "terminal") return Promise.resolve(this.snapshot);
    if (this.snapshot.phase === "idle" && !this.snapshot.target && !this.operations.pending() && !this.submitPromise) {
      this.publish({ feedback: "stoppedBeforeDispatch", error: null });
      return Promise.resolve(this.snapshot);
    }
    this.publish({ stopRequested: true, feedback: this.actionPromise ? "stopInProgress" : "stopRecorded", error: null });
    this.persistStop();
    if (this.actionPromise) return this.actionPromise;
    if (this.snapshot.target) return this.cancelTarget();
    if (!this.dispatched && !this.hadPendingSubmission && (this.submitPromise || this.snapshot.phase === "preparing")) {
      const operationId = this.snapshot.operationId;
      const pending = this.operations.pending();
      if (pending && (pending.operationId !== operationId || !this.operations.complete)) return Promise.resolve(this.snapshot);
      if (pending && operationId) this.operations.complete?.(operationId);
      this.submissionEpoch += 1;
      const rejectPreparation = this.preparationStop;
      this.submitPromise = null; this.preparationStop = null;
      this.storage.removeItem(this.storageKey);
      this.publish({ phase: "idle", operationId: null, stopRequested: false, feedback: "stoppedBeforeDispatch", error: null });
      rejectPreparation?.(new Error("operation_dispatch_stopped"));
      return Promise.resolve(this.snapshot);
    }
    if (this.submitPromise || this.snapshot.phase === "preparing") return Promise.resolve(this.snapshot);
    return this.recover();
  }

  /** Recovery only reads the original identity. It cannot resubmit a message. */
  recover(): Promise<SubmissionSnapshot> {
    if (this.actionPromise) return this.actionPromise;
    if (this.snapshot.phase === "terminal") return Promise.resolve(this.snapshot);
    if (this.snapshot.target && this.snapshot.stopRequested) return this.cancelTarget();
    const attempt = this.snapshot.attemptId;
    const operationId = this.snapshot.operationId ?? this.operations.pending()?.operationId;
    const owns = () => this.snapshot.attemptId === attempt;
    const pending = Promise.resolve().then(async () => {
      if (!operationId || this.operations.pending()?.operationId !== operationId) throw new Error("operation_result_stale");
      const receipt = await this.operations.recover();
      if (!owns()) return this.snapshot;
      if (!receipt || receipt.operationId !== operationId) throw new Error("operation_result_stale");
      const target = { sessionId: receipt.sessionId, agentRunId: receipt.agentRunId! };
      if (!isTarget(target)) throw new Error("operation_receipt_invalid");
      const terminal = this.terminals.get(keyOf(target)) ?? null;
      this.publish({ operationId, target, terminal, phase: terminal ? "terminal" : "accepted", error: null, feedback: terminal ? "terminal" : null });
      this.persistStop();
      if (this.snapshot.stopRequested && !terminal) {
        if (this.actionPromise === pending) this.actionPromise = null;
        return this.cancelTarget();
      }
      return this.snapshot;
    }).catch(error => {
      if (owns() && this.snapshot.phase !== "terminal") {
        if (isExplicitRejection(error)) this.rejectStop(error, false);
        else { this.publish({ phase: "unknown", error, feedback: this.snapshot.stopRequested ? "stopUnconfirmed" : null }); this.persistStop(); }
      }
      return this.snapshot;
    }).finally(() => { if (this.actionPromise === pending) this.actionPromise = null; });
    this.actionPromise = pending;
    this.publish({ phase: "recovering", operationId: operationId ?? null });
    return pending;
  }

  private cancelTarget(): Promise<SubmissionSnapshot> {
    if (this.actionPromise) return this.actionPromise;
    const target = this.snapshot.target;
    if (!target || this.snapshot.phase === "terminal") return Promise.resolve(this.snapshot);
    const attempt = this.snapshot.attemptId;
    const preserveIntent = this.retainCancellation;
    const owns = () => this.snapshot.attemptId === attempt && sameTarget(this.snapshot.target, target);
    const pending = Promise.resolve().then(() => this.cancel(target)).then(result => {
      if (!owns() || this.snapshot.phase === "terminal") return this.snapshot;
      const statuses = result?.disposition === "terminal" ? ["completed", "failed", "cancelled"] : ["queued", "running"];
      if (!result || !exactKeys(result, ["agentRunId", "status", "disposition"]) || result.agentRunId !== target.agentRunId
        || !["requested", "terminal"].includes(result.disposition) || !statuses.includes(result.status)) throw new Error("agent_run_cancel_response_invalid");
      if (result.disposition === "terminal") this.observeTerminal({ ...target, status: result.status as TerminalStatus });
      else { this.retainCancellation = true; this.publish({ phase: "stopRequested", feedback: "stopRequested", error: null }); this.persistStop(); }
      return this.snapshot;
    }).catch(error => {
      if (owns() && this.snapshot.phase !== "terminal") {
        if (isExplicitRejection(error)) this.rejectStop(error, preserveIntent);
        else { this.retainCancellation = true; this.publish({ phase: "unknown", feedback: "stopUnconfirmed", error }); this.persistStop(); }
      }
      return this.snapshot;
    }).finally(() => { if (this.actionPromise === pending) this.actionPromise = null; });
    this.actionPromise = pending;
    this.publish({ phase: "cancelling", feedback: "stopInProgress", error: null });
    return pending;
  }

  private rejectStop(error: unknown, preserveIntent: boolean) {
    this.retainCancellation = preserveIntent;
    const feedback = this.snapshot.stopRequested || preserveIntent ? "stopRejected" : "submissionRejected";
    this.publish({ phase: "rejected", feedback, stopRequested: preserveIntent, error });
    if (preserveIntent) this.persistStop();
    else this.storage.removeItem(this.storageKey);
  }

  /** A denied retry cannot disprove an earlier dispatched cancellation. Keep
   * that target's control record available to its independent Run actor before
   * a new submission uses the workspace's command slot. This does not retry. */
  private preserveRejectedStop() {
    const { target, operationId, terminal } = this.snapshot;
    if (!target) throw new Error("submission_stop_pending");
    const key = `centaeris.submissionControl.v1:${JSON.stringify([...this.scopeIdentity, keyOf(target)])}`;
    if (key === this.storageKey) throw new Error("submission_stop_pending");
    const existing = this.storage.getItem(key);
    if (existing === null) {
      const value: Stored = { schema: "web.submission_control.v1", operationId, target, stopRequested: true, terminal };
      this.storage.setItem(key, JSON.stringify(value));
    } else {
      const value = parseStored(existing);
      if (!sameTarget(value.target, target)) throw new Error("submission_control_storage_invalid");
    }
  }

  /** This input must come from an authoritative current-Run observation. A null
   * observation cannot turn a previously accepted Run into a terminal fact. */
  setTarget(target: SubmissionTarget | null) {
    if (!target) return false;
    if (!isTarget(target)) throw new Error("submission_target_invalid");
    if (this.terminals.has(keyOf(target))) return false;
    if (sameTarget(this.snapshot.target, target)) return true;
    if (this.snapshot.phase !== "terminal" && (this.snapshot.stopRequested || this.submitPromise || this.actionPromise
      || (!["idle", "accepted", "rejected"].includes(this.snapshot.phase) && !(this.snapshot.phase === "unknown" && !this.operations.pending())))) return false;
    this.storage.removeItem(this.storageKey);
    this.submitPromise = null; this.actionPromise = null; this.preparationStop = null;
    this.submissionEpoch += 1; this.hadPendingSubmission = false; this.dispatched = false; this.retainCancellation = false;
    this.publish({ attemptId: this.snapshot.attemptId + 1, phase: "accepted", target: { ...target }, operationId: null,
      stopRequested: false, feedback: null, error: null, terminal: null });
    return true;
  }

  observeTerminal(target: SubmissionTarget & { status?: TerminalStatus; completedAtMs?: number }) {
    const identity = { sessionId: target.sessionId, agentRunId: target.agentRunId };
    const terminal: Terminal = { status: target.status ?? "cancelled", ...(target.completedAtMs === undefined ? {} : { completedAtMs: target.completedAtMs }) };
    if (!isTarget(identity) || !isTerminal(terminal)) throw new Error("submission_terminal_invalid");
    if (!this.terminals.has(keyOf(identity))) this.terminals.set(keyOf(identity), terminal);
    if (!sameTarget(this.snapshot.target, identity) || this.snapshot.phase === "terminal") return;
    this.publish({ phase: "terminal", terminal: this.terminals.get(keyOf(identity))!, feedback: "terminal", error: null });
    this.persistStop();
  }
}
