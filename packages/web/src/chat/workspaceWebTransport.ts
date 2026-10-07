import { ApiError, apiResponse } from "../api.ts";
import { abortableDelay } from "./abortableDelay.ts";
import { readSse } from "./sessionStreamProtocol.ts";
import type { StreamEntry } from "./streamTypes.ts";

type WorkspaceStreamController = {
  sessionId: string;
  agentRunId: string;
  lastCursor: string;
  failureSignal?: AbortSignal;
  acceptWithBackpressure(entry: StreamEntry): Promise<void>;
  whenIdle(): Promise<void>;
  setCursor(cursor: string): void;
};

type StreamWorkspaceAgentRunOptions = {
  controller: WorkspaceStreamController;
  signal: AbortSignal;
  onConnection(connection: "running" | "reconnecting"): void;
  request?: typeof apiResponse;
  wait?: typeof abortableDelay;
  recover?(error: unknown): Promise<WorkspaceStreamController | null>;
};

export async function streamWorkspaceAgentRun({
  controller,
  signal,
  onConnection,
  request = apiResponse,
  wait = abortableDelay,
  recover,
}: StreamWorkspaceAgentRunOptions) {
  let failures = 0;
  let pendingRecovery: unknown = null;
  while (!signal.aborted) {
    try {
      if (pendingRecovery !== null && recover) {
        const next = await recover(pendingRecovery);
        if (next === null) return;
        controller = next;
        pendingRecovery = null;
      }
      const attemptSignal = controller.failureSignal
        ? AbortSignal.any([signal, controller.failureSignal]) : signal;
      const response = await request(
        `/api/sessions/${controller.sessionId}/agent-runs/${controller.agentRunId}/events`,
        {
          signal: attemptSignal,
          headers: controller.lastCursor !== "0-0" ? { "Last-Event-ID": controller.lastCursor } : {},
        },
      );
      onConnection("running");
      const terminal = await readSse(
        response,
        controller.agentRunId,
        async (entry: StreamEntry) => {
          await controller.acceptWithBackpressure(entry);
        },
        { signal: attemptSignal },
      );
      await controller.whenIdle();
      if (terminal || signal.aborted) {
        return;
      }
      failures = 0;
    } catch (error: unknown) {
      if (signal.aborted) throw new DOMException("Aborted", "AbortError");
      // A failed frame must wake the SSE reader even if no more tokens arrive.
      if (error instanceof Error && error.name === "AbortError" && controller.failureSignal?.aborted) {
        error = controller.failureSignal.reason;
      }
      if (error instanceof ApiError && ([401, 403, 404].includes(error.status)
        || error.message === "agent_run_not_admitted")) throw error;
      if (++failures > 5) throw error;
      if (recover) pendingRecovery = error;
      else if (!(error instanceof TypeError)
        && !(error instanceof ApiError && (error.status === 429 || error.status >= 500))) throw error;
    }
    onConnection("reconnecting");
    await wait(Math.min(500 * 2 ** Math.max(0, failures - 1), 8000), signal);
  }
}
