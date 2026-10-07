import { ApiError, apiJson } from "../api.ts";
import type { ActiveTranscriptAgentRun, createWorkspaceTranscriptTransport } from "./transcriptTransport.ts";
import type { TranscriptViewStore } from "./transcriptViewStore.ts";

type Command = "createSession" | "submitMessage";
type Scope = { userId: string; workspaceId: string; command: Command };
type Receipt = { operationId: string; command: Command; status: "accepted"; sessionId: string; agentRunId: string | null; turnId: string | null };
type Pending = { operationId: string; fingerprint: string; assetIds: string[]; inputChanged: boolean; receipt: Receipt | null };
type Storage = Pick<globalThis.Storage, "getItem" | "setItem" | "removeItem">;
type Request = (path: string, options?: RequestInit) => Promise<unknown>;

function canonical(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canonical);
  if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).filter(([, item]) => item !== undefined).sort(([left], [right]) => left.localeCompare(right)).map(([key, item]) => [key, canonical(item)]));
  return value;
}

async function digest(bytes: ArrayBuffer): Promise<string> {
  return Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function receipt(value: unknown, command: Command, operationId: string): Receipt {
  const item = value as Receipt;
  if (!item || typeof item !== "object"
    || Object.keys(item).sort().join("|") !== "agentRunId|command|operationId|sessionId|status|turnId"
    || item.operationId !== operationId || item.command !== command || item.status !== "accepted"
    || typeof item.sessionId !== "string" || !item.sessionId
    || (command === "createSession" ? item.agentRunId !== null || item.turnId !== null
      : typeof item.agentRunId !== "string" || !item.agentRunId || typeof item.turnId !== "string" || !item.turnId || item.agentRunId === item.turnId)) {
    throw new Error("operation_receipt_invalid");
  }
  return item;
}

function samePendingVersion(current: Pending | null, expected: Pending) {
  return current?.operationId === expected.operationId && current.fingerprint === expected.fingerprint && current.inputChanged === expected.inputChanged;
}

/** A tab retains uncertain command identities across route changes and reloads.
 * Stored metadata contains no message, file bytes, or credentials. */
export class OperationClient {
  private scope: Scope;
  private storage: Storage;
  private request: Request;
  private key: string;

  constructor(scope: Scope, options: { storage?: Storage; request?: Request } = {}) {
    this.scope = scope;
    this.storage = options.storage ?? sessionStorage;
    this.request = options.request ?? apiJson;
    this.key = `centaeris.pendingOperation.v1:${JSON.stringify([scope.userId, scope.workspaceId, scope.command])}`;
  }

  pending(): Pending | null {
    const stored = this.storage.getItem(this.key);
    if (stored === null) return null;
    const item = JSON.parse(stored) as Pending;
    if (!item || Object.keys(item).sort().join("|") !== "assetIds|fingerprint|inputChanged|operationId|receipt"
      || typeof item.operationId !== "string" || !/^[A-Za-z0-9_.:-]{1,128}$/.test(item.operationId)
      || typeof item.fingerprint !== "string" || !/^[a-f0-9]{64}$/.test(item.fingerprint)
      || typeof item.inputChanged !== "boolean"
      || !Array.isArray(item.assetIds) || item.assetIds.some((id) => typeof id !== "string")) throw new Error("operation_storage_invalid");
    if (item.receipt !== null) receipt(item.receipt, this.scope.command, item.operationId);
    return item;
  }

  complete(operationId: string) {
    if (this.pending()?.operationId === operationId) this.storage.removeItem(this.key);
  }

  async recover(): Promise<Receipt | null> {
    const item = this.pending();
    if (!item) return null;
    return this.lookup(item);
  }

  private async lookup(item: Pending): Promise<Receipt> {
    // Always reauthorize through the server, even when acceptance was already observed.
    const result = receipt(await this.request(`/api/workspaces/${encodeURIComponent(this.scope.workspaceId)}/operations/${this.scope.command}/${encodeURIComponent(item.operationId)}`), this.scope.command, item.operationId);
    this.update(item, { receipt: result });
    return result;
  }

  private save(item: Pending) { this.storage.setItem(this.key, JSON.stringify(item)); }

  private update(expected: Pending, changes: Partial<Pending>) {
    const current = this.pending();
    if (!samePendingVersion(current, expected) || !current) throw new Error("operation_result_stale");
    this.save({ ...current, ...changes });
  }

  async submit(path: string, input: Record<string, unknown>, files: File[] = [], assetIds: string[] = [], replaceUnconfirmedInput = false): Promise<Receipt> {
    const uploads = [];
    for (const file of files) uploads.push({ name: file.name, type: file.type, digest: await digest(await file.arrayBuffer()) });
    const fingerprint = await digest(new TextEncoder().encode(JSON.stringify(canonical({ path, input, uploads, assetIds }))).buffer);
    let item = this.pending();
    const previouslyPending = item !== null;
    const replacing = item !== null && item.fingerprint !== fingerprint && replaceUnconfirmedInput && item.receipt === null && this.scope.command === "submitMessage";
    if (item) {
      if (item.fingerprint !== fingerprint && !replacing) throw new Error("operation_pending_input_changed");
      if (replacing) {
        this.update(item, { inputChanged: true });
        item = { ...item, inputChanged: true };
      }
      try {
        const found = await this.lookup(item);
        if (found) {
          if (replacing || item.inputChanged) throw new Error("operation_input_acceptance_unconfirmed");
          return found;
        }
      } catch (error) {
        if (!(error instanceof ApiError && error.status === 404 && error.message === "operation_not_found") || item.receipt) throw error;
      }
      if (replacing) {
        this.update(item, { fingerprint, inputChanged: true });
        item = { ...item, fingerprint, inputChanged: true };
      }
    } else {
      item = { operationId: crypto.randomUUID(), fingerprint, assetIds: [...assetIds], inputChanged: false, receipt: null };
      // Storage failure stops the request before the server can accept it.
      this.save(item);
    }
    if (!samePendingVersion(this.pending(), item)) throw new Error("operation_result_stale");
    let body: BodyInit;
    if (files.length) {
      const form = new FormData();
      for (const [key, value] of Object.entries({ ...input, operationId: item.operationId })) {
        if (value !== undefined) form.append(key, typeof value === "string" ? value : JSON.stringify(value));
      }
      files.forEach((file) => form.append("files", file));
      body = form;
    } else body = JSON.stringify({ ...input, operationId: item.operationId });
    try {
      const result = receipt(await this.request(path, { method: "POST", body }), this.scope.command, item.operationId);
      this.update(item, { receipt: result, inputChanged: false });
      return result;
    } catch (error) {
      if (item.inputChanged && error instanceof ApiError && error.status === 409 && error.message === "operation_conflict") {
        await this.lookup(item);
        throw new Error("operation_input_acceptance_unconfirmed");
      }
      const current = this.pending();
      if (!previouslyPending && error instanceof ApiError && [400, 422, 429].includes(error.status)
        && samePendingVersion(current, item) && current?.receipt === null) this.complete(item.operationId);
      // A response lost after admission is recovered by identity, never by a
      // second POST. Keep corrected-input ambiguity and stale-tab guards intact.
      if (!item.inputChanged && samePendingVersion(current, item)
        && (error instanceof TypeError || (error instanceof ApiError && error.status >= 500))) {
        try { return await this.lookup(item); }
        catch (recoveryError) {
          const unavailable = recoveryError instanceof TypeError || (recoveryError instanceof ApiError
            && ((recoveryError.status === 404 && recoveryError.message === "operation_not_found") || recoveryError.status >= 500));
          if (!unavailable) throw recoveryError;
        }
      }
      throw error;
    }
  }
}

/** A receipt locates a Session; its Run may already have finished. Subscribe to
 * the current run discovered by the read API, including a later run. */
export async function loadAcceptedConversation(sessionId: string, options: {
  transport: Pick<ReturnType<typeof createWorkspaceTranscriptTransport>, "loadTail" | "loadActiveAgentRun">;
  store: Pick<TranscriptViewStore, "openTail">;
  setActive: (run: ActiveTranscriptAgentRun | null) => void;
  isCurrent: () => boolean;
}, signal: AbortSignal) {
  const tail = await options.transport.loadTail(sessionId, signal);
  const identity = { sessionId, projectionVersion: tail.projectionVersion, projectionGeneration: tail.projectionGeneration };
  const envelope = await options.transport.loadActiveAgentRun({ ...identity, sourceHighWater: tail.sourceHighWater }, signal);
  if (signal.aborted || !options.isCurrent()) throw new DOMException("Aborted", "AbortError");
  const viewEpoch = options.store.openTail(tail);
  options.setActive(envelope.agentRun);
  return envelope.agentRun ? { identity, viewEpoch, cursor: envelope.agentRun.streamCursor, agentRunId: envelope.agentRun.agentRunId } : null;
}

export function acceptedConversationReviewLink(workspaceId: string, session: { id: string; agentId: string }) {
  return {
    href: `/w/${encodeURIComponent(workspaceId)}/agents/${encodeURIComponent(session.agentId)}?sessionId=${encodeURIComponent(session.id)}`,
    target: "_blank",
    rel: "noopener",
  };
}

/** Called by the user's new-tab link click. It only consumes the confirmed
 * operation metadata; the source page's draft and files remain in memory. */
export async function consumeReviewedOperation(client: OperationClient, operationId: string, readSession: (id: string) => Promise<{ id: string; agentId: string }>) {
  const expected = client.pending();
  if (!expected || expected.operationId !== operationId) throw new Error("operation_result_stale");
  const accepted = await client.recover();
  if (!accepted || accepted.operationId !== operationId) throw new Error("operation_result_stale");
  const session = await readSession(accepted.sessionId);
  if (session?.id !== accepted.sessionId || typeof session.agentId !== "string" || !session.agentId) throw new Error("session_response_invalid");
  if (!samePendingVersion(client.pending(), expected)) throw new Error("operation_result_stale");
  client.complete(operationId);
}
