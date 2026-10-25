import type { SessionStreamEvent, UnknownRecord } from "./streamTypes.ts";
import type { PresentationOperation } from "./workspacePresentationProtocol.ts";
import { canonicalTranscriptValue } from "./transcriptContract.ts";
import { validateOperation } from "./sessionEvents.ts";

const FAILED_RESULT_STATES = new Set(["failed", "denied", "aborted"]);

export type ToolOperationRecord = Readonly<{
  callId: string;
  call: UnknownRecord;
  result: UnknownRecord | null;
  operations: readonly UnknownRecord[];
  status: "running" | "completed" | "failed";
  previewTruncated?: boolean;
  detailSourceEventId?: string;
}>;

export type ToolOperationSnapshot = Readonly<{
  version: number;
  records: ReadonlyMap<string, ToolOperationRecord>;
}>;

const EMPTY_SNAPSHOT: ToolOperationSnapshot = { version: 0, records: new Map() };

// Operational details augment the transcript projection. Both native committed
// events and hosted presentation records retain their own protocol boundaries.
export class TranscriptToolOperationRegistry {
  private version = 0;
  private snapshot: ToolOperationSnapshot = EMPTY_SNAPSHOT;
  private readonly records = new Map<string, ToolOperationRecord>();
  private readonly receivedDetails = new Map<string, string>();
  private readonly listeners = new Set<() => void>();

  getSnapshot = () => this.snapshot;

  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  };

  clear() {
    this.receivedDetails.clear();
    if (this.records.size === 0) return;
    this.records.clear();
    this.publish();
  }

  applyPresentationDetail(detail: PresentationOperation) {
    const canonical = canonicalTranscriptValue(detail);
    const previous = this.receivedDetails.get(detail.sourceEventId);
    if (previous !== undefined) {
      if (previous !== canonical) throw new Error("conflicting presentation operation replay");
      return;
    }
    this.applyPayload(detail.kind === "call" ? "tool_call" : "tool_result", {
      callId: detail.callId, toolName: detail.toolName,
      normalizedInput: detail.normalizedInput, resultState: detail.resultState,
      operations: detail.operations,
    }, detail);
    this.receivedDetails.set(detail.sourceEventId, canonical);
  }

  applyEvent(event: SessionStreamEvent) { this.applyPayload(event.type, event.payload); }

  private applyPayload(type: string, payload: UnknownRecord, detail?: PresentationOperation) {
    const presentation = detail ? { previewTruncated: detail.previewTruncated, detailSourceEventId: detail.sourceEventId } : {};
    if (type === "tool_call") {
      const callId = payload.callId;
      if (typeof callId !== "string" || !callId) return;
      this.records.set(callId, {
        callId,
        call: { ...payload },
        result: null,
        operations: [],
        status: "running",
        ...presentation,
      });
      this.publish();
      return;
    }
    if (type === "tool_result") {
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
        ...presentation,
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
