import { ApiError, apiResponse } from "../api.ts";
import {
  validateTranscriptPage,
  validateTranscriptPatchPage,
  type TranscriptPage,
  type TranscriptPatchPage,
} from "./transcriptViewStore.ts";
import {
  validateTranscriptContentRange,
  type TranscriptContentRef,
  type TranscriptContentRange,
} from "./transcriptContract.ts";

const PROJECTION_RETRY_DELAY_MS = 50;

type Request = typeof apiResponse;
type Wait = (delayMs: number, signal: AbortSignal) => Promise<void>;
type ApplyPatchPage = (page: TranscriptPatchPage) => void;

type TranscriptReadIdentity = Readonly<{
  sessionId: string;
  projectionVersion: string;
  projectionGeneration: string;
  sourceHighWater: string;
}>;

type TranscriptPatchIdentity = Omit<TranscriptReadIdentity, "sourceHighWater"> & Readonly<{
  afterSourceHighWater: string;
}>;

export type ActiveTranscriptAgentRun = Readonly<{
  agentRunId: string;
  status: "queued" | "running";
  streamCursor: string;
}>;

export type ActiveTranscriptAgentRunEnvelope = Readonly<{
  schema: "workspace.transcript.active_agent_run.v1";
  sessionId: string;
  agentRun: ActiveTranscriptAgentRun | null;
}>;

function abortableWait(delayMs: number, signal: AbortSignal) {
  return new Promise<void>((resolve, reject) => {
    if (signal.aborted) {
      reject(new DOMException("Aborted", "AbortError"));
      return;
    }
    const timeoutId = setTimeout(() => {
      signal.removeEventListener("abort", onAbort);
      resolve();
    }, delayMs);
    const onAbort = () => {
      clearTimeout(timeoutId);
      reject(new DOMException("Aborted", "AbortError"));
    };
    signal.addEventListener("abort", onAbort, { once: true });
  });
}

function pathWithQuery(path: string, entries: readonly (readonly [string, string])[]) {
  const query = new URLSearchParams(entries.map(([key, value]) => [key, value]));
  return query.size > 0 ? `${path}?${query}` : path;
}

function projectionProgress(error: unknown, expectedGeneration?: string) {
  if (!(error instanceof ApiError) || error.status !== 409
    || error.message !== "transcript_projection_not_ready"
    || typeof error.payload !== "object" || error.payload === null) return null;
  const payload = error.payload as Record<string, unknown>;
  if (Object.keys(payload).sort().join("|") !== "error|projectedHighWater|projectionGeneration|sourceHighWater"
    || payload.error !== "transcript_projection_not_ready"
    || typeof payload.projectionGeneration !== "string" || !payload.projectionGeneration
    || (expectedGeneration !== undefined && payload.projectionGeneration !== expectedGeneration)
    || typeof payload.sourceHighWater !== "string" || !/^(0|[1-9][0-9]*)$/.test(payload.sourceHighWater)
    || typeof payload.projectedHighWater !== "string" || !/^(0|[1-9][0-9]*)$/.test(payload.projectedHighWater)
    || BigInt(payload.projectedHighWater) >= BigInt(payload.sourceHighWater)) return null;
  return {
    projectionGeneration: payload.projectionGeneration,
    sourceHighWater: payload.sourceHighWater,
  };
}

function validateActiveAgentRun(value: unknown, sessionId: string): ActiveTranscriptAgentRunEnvelope {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error("invalid active transcript AgentRun");
  }
  const envelope = value as Record<string, unknown>;
  if (Object.keys(envelope).sort().join("|") !== "agentRun|schema|sessionId"
    || envelope.schema !== "workspace.transcript.active_agent_run.v1"
    || envelope.sessionId !== sessionId) throw new Error("invalid active transcript AgentRun");
  if (envelope.agentRun === null) return envelope as ActiveTranscriptAgentRunEnvelope;
  if (typeof envelope.agentRun !== "object" || Array.isArray(envelope.agentRun)) {
    throw new Error("invalid active transcript AgentRun");
  }
  const run = envelope.agentRun as Record<string, unknown>;
  if (Object.keys(run).sort().join("|") !== "agentRunId|status|streamCursor"
    || typeof run.agentRunId !== "string" || !run.agentRunId
    || !["queued", "running"].includes(String(run.status))
    || typeof run.streamCursor !== "string" || !run.streamCursor) {
    throw new Error("invalid active transcript AgentRun");
  }
  return structuredClone(envelope) as ActiveTranscriptAgentRunEnvelope;
}

export function createWorkspaceTranscriptTransport({
  request = apiResponse,
  wait = abortableWait,
}: Readonly<{ request?: Request; wait?: Wait }> = {}) {
  async function json(path: string, signal: AbortSignal) {
    return (await request(path, { signal })).json() as Promise<unknown>;
  }

  async function loadTail(sessionId: string, signal: AbortSignal): Promise<TranscriptPage> {
    let frozen: { sourceHighWater: string; projectionGeneration: string } | null = null;
    while (true) {
      const path = pathWithQuery(`/api/sessions/${encodeURIComponent(sessionId)}/transcript`, frozen ? [
        ["sourceHighWater", frozen.sourceHighWater],
        ["projectionGeneration", frozen.projectionGeneration],
      ] : []);
      try {
        return validateTranscriptPage(await json(path, signal), { sessionId });
      } catch (error) {
        const progress = projectionProgress(error, frozen?.projectionGeneration);
        if (progress !== null) frozen = progress;
        else if (error instanceof ApiError && error.status === 409
          && error.message === "transcript_view_invalidated") frozen = null;
        else throw error;
        await wait(PROJECTION_RETRY_DELAY_MS, signal);
      }
    }
  }

  async function loadOlder(
    identity: TranscriptReadIdentity,
    olderCursor: string,
    signal: AbortSignal,
  ): Promise<TranscriptPage> {
    const path = pathWithQuery(`/api/sessions/${encodeURIComponent(identity.sessionId)}/transcript`, [
      ["sourceHighWater", identity.sourceHighWater],
      ["projectionGeneration", identity.projectionGeneration],
      ["olderCursor", olderCursor],
    ]);
    const page = validateTranscriptPage(await json(path, signal), { sessionId: identity.sessionId });
    if (page.projectionVersion !== identity.projectionVersion
      || page.projectionGeneration !== identity.projectionGeneration
      || page.sourceHighWater !== identity.sourceHighWater) throw new Error("transcript page identity changed");
    return page;
  }

  async function loadPatches(
    identity: TranscriptPatchIdentity,
    signal: AbortSignal,
    applyPage: ApplyPatchPage,
  ): Promise<string> {
    let after = identity.afterSourceHighWater;
    let through: string | null = null;
    while (true) {
      const entries: [string, string][] = [
        ["afterSourceHighWater", after],
        ...(through === null ? [] : [["throughSourceHighWater", through] as [string, string]]),
        ["projectionGeneration", identity.projectionGeneration],
      ];
      const path = pathWithQuery(
        `/api/sessions/${encodeURIComponent(identity.sessionId)}/transcript/patches`,
        entries,
      );
      let page: TranscriptPatchPage;
      try {
        page = validateTranscriptPatchPage(await json(path, signal), {
          sessionId: identity.sessionId,
          projectionVersion: identity.projectionVersion,
          projectionGeneration: identity.projectionGeneration,
          sourceHighWater: through || after,
          afterSourceHighWater: after,
        });
      } catch (error) {
        const progress = projectionProgress(error, identity.projectionGeneration);
        if (progress === null) throw error;
        if (through !== null && progress.sourceHighWater !== through) {
          throw new Error("transcript patch through-waterline changed");
        }
        through = progress.sourceHighWater;
        await wait(PROJECTION_RETRY_DELAY_MS, signal);
        continue;
      }
      through ??= page.throughSourceHighWater;
      if (page.throughSourceHighWater !== through) throw new Error("transcript patch through-waterline changed");
      applyPage(page);
      if (!page.hasMore) return page.nextSourceHighWater;
      after = page.nextSourceHighWater;
    }
  }

  async function loadActiveAgentRun(
    identity: TranscriptReadIdentity,
    signal: AbortSignal,
  ): Promise<ActiveTranscriptAgentRunEnvelope> {
    const path = pathWithQuery(
      `/api/sessions/${encodeURIComponent(identity.sessionId)}/transcript/active-agent-run`,
      [["sourceHighWater", identity.sourceHighWater]],
    );
    return validateActiveAgentRun(await json(path, signal), identity.sessionId);
  }

  async function loadContentRange(
    identity: Readonly<{
      sessionId: string;
      projectionGeneration: string;
      reference: TranscriptContentRef;
    }>,
    offset: string,
    signal: AbortSignal,
  ): Promise<TranscriptContentRange> {
    const path = pathWithQuery(
      `/api/sessions/${encodeURIComponent(identity.sessionId)}/transcript/content`,
      [
        ["projectionGeneration", identity.projectionGeneration],
        ["refId", identity.reference.refId],
        ["revision", identity.reference.revision],
        ["byteLength", identity.reference.byteLength],
        ["offset", offset],
      ],
    );
    return validateTranscriptContentRange(await json(path, signal), { ...identity, offset });
  }

  return { loadTail, loadOlder, loadPatches, loadActiveAgentRun, loadContentRange };
}
