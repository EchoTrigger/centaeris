import type { SessionStreamEvent, UnknownRecord } from "./streamTypes.ts";
import { validateOperation } from "./sessionEvents.ts";

const FAILED_RESULT_STATES = new Set(["failed", "denied", "aborted"]);

export type ToolOperationRecord = Readonly<{
  callId: string;
  call: UnknownRecord;
  result: UnknownRecord | null;
  operations: readonly UnknownRecord[];
  status: "running" | "completed" | "failed";
}>;

export type ToolOperationSnapshot = Readonly<{
  version: number;
  records: ReadonlyMap<string, ToolOperationRecord>;
}>;

const EMPTY_SNAPSHOT: ToolOperationSnapshot = { version: 0, records: new Map() };

// Event-derived operational detail (command, per-operation results, diffs).
// The paged transcript projection intentionally omits these, so they are only
// available for runs whose committed session events have been streamed.
export class TranscriptToolOperationRegistry {
  private version = 0;
  private snapshot: ToolOperationSnapshot = EMPTY_SNAPSHOT;
  private readonly records = new Map<string, ToolOperationRecord>();
  private readonly listeners = new Set<() => void>();

  getSnapshot = () => this.snapshot;

  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  };

  clear() {
    if (this.records.size === 0) return;
    this.records.clear();
    this.publish();
  }

  applyEvent(event: SessionStreamEvent) {
    const payload = event.payload;
    if (event.type === "tool_call") {
      const callId = payload.callId;
      if (typeof callId !== "string" || !callId) return;
      this.records.set(callId, {
        callId,
        call: { ...payload },
        result: null,
        operations: [],
        status: "running",
      });
      this.publish();
      return;
    }
    if (event.type === "tool_result") {
      const callId = payload.callId;
      if (typeof callId !== "string" || !callId) return;
      const existing = this.records.get(callId);
      const operations = Array.isArray(payload.operations)
        ? payload.operations.map((operation) => validateOperation(operation, callId))
        : [];
      const failed = typeof payload.resultState === "string"
        && FAILED_RESULT_STATES.has(payload.resultState);
      this.records.set(callId, {
        callId,
        call: existing?.call ?? {},
        result: { ...payload, operations },
        operations,
        status: failed ? "failed" : "completed",
      });
      this.publish();
    }
  }

  private publish() {
    this.version += 1;
    this.snapshot = { version: this.version, records: new Map(this.records) };
    this.listeners.forEach((listener) => listener());
  }
}
