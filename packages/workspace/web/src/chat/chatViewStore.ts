import {
  applyStreamEntry,
  isAgentRunActive,
  type ChatMessage,
  type ProjectedAgentRun,
} from "./sessionEvents.ts";
import type { StreamEntry, UnknownRecord } from "./streamTypes.ts";
import type { CitationSnapshot } from "./citationSnapshot.ts";

type Listener = () => void;
type ChangeListener = (agentRunId: string) => void;

export type StreamTelemetry = {
  entry: StreamEntry;
  acceptedAt: number;
  acceptedOrdinal: number;
  agentRunId: string;
  streamItemKind: StreamEntry["item"]["kind"];
};

type PendingRenderTelemetry = StreamTelemetry & {
  renderRevision: number;
  reducerAppliedAt: number;
  domCommitAt?: number;
};

type ScheduledRenderTelemetry = {
  frameId: number;
  telemetry: PendingRenderTelemetry;
};

export type RenderTelemetryMetric = Readonly<{
  schema: "workspace.chat_render_telemetry.v1";
  agentRunId: string;
  acceptedOrdinal: number;
  streamItemKind: StreamEntry["item"]["kind"];
  acceptedAt: number;
  reducerAppliedAt: number;
  domCommitAt: number;
  domPaintBoundaryAt: number;
  acceptedToReducerMs: number;
  reducerToDomCommitMs: number;
  domCommitToPaintBoundaryMs: number;
  acceptedToDomPaintBoundaryMs: number;
}>;

type ChatViewStoreOptions = {
  now?: () => number;
  monotonicNow?: () => number;
  scheduleFrame?: (callback: () => void) => number;
  cancelFrame?: (frameId: number) => void;
  onRenderTelemetry?: (metric: RenderTelemetryMetric) => void;
};

type ChatListSnapshot = Readonly<{
  agentRunIds: readonly string[];
  hasActiveAgentRun: boolean;
}>;

export type ChatViewStore = {
  getListSnapshot(): ChatListSnapshot;
  getAgentRunSnapshot(agentRunId: string): ProjectedAgentRun | null;
  getMessageSnapshot(messageId: string): ChatMessage | null;
  getMessageIdsForAgentRun(agentRunId: string): readonly string[];
  getStreamRenderRevision(agentRunId: string): number;
  getActivityDisclosures(agentRunId: string): ReadonlySet<string>;
  toggleActivityDisclosure(agentRunId: string, identity: string): void;
  subscribeList(listener: Listener): () => boolean;
  subscribeAgentRun(agentRunId: string, listener: Listener): () => boolean | void;
  subscribeChanges(listener: ChangeListener): () => boolean;
  clear(): void;
  replaceAll(agentRuns: readonly ProjectedAgentRun[]): void;
  prependAgentRuns(agentRuns: readonly ProjectedAgentRun[]): void;
  appendAgentRun(agentRun: ProjectedAgentRun): void;
  replaceAgentRun(agentRun: ProjectedAgentRun): void;
  applyCitationSnapshot(snapshot: CitationSnapshot): void;
  replaceAgentRunId(previousAgentRunId: string, agentRun: ProjectedAgentRun): void;
  updateConnection(agentRunId: string, connection: string): void;
  rejectPendingAgentRun(agentRunId: string): void;
  markAgentRunProjectionError(agentRunId: string): void;
  beginStreamTelemetry(entry: StreamEntry, acceptedOrdinal: number): Readonly<StreamTelemetry>;
  applyStreamEntries(
    entries: readonly StreamEntry[],
    telemetry?: readonly (Readonly<StreamTelemetry> | undefined)[],
  ): void;
  markDomCommit(agentRunId: string, renderRevision: number): (() => void) | undefined;
};

function cloneUnknownObject(value: unknown): UnknownRecord {
  return { ...(value as UnknownRecord) };
}

function displayPhaseKey(agentRun: ProjectedAgentRun) {
  return agentRun.connection === "reconnecting" ? "reconnecting" : agentRun.phaseKey || "thinking";
}

function cloneAgentRun(agentRun: ProjectedAgentRun): ProjectedAgentRun {
  return {
    ...agentRun,
    reasoningBlocks: agentRun.reasoningBlocks.map((block) => ({ ...block })),
    events: (agentRun.events || []).map((stored) => ({ ...stored, event: { ...stored.event, payload: { ...stored.event.payload } } })),
    eventIds: [...(agentRun.eventIds || [])],
    live: agentRun.live ? { ...agentRun.live } : null,
    projectionError: agentRun.projectionError || null,
    messages: (agentRun.messages || []).map((message) => ({
      ...message,
      attachments: (message.attachments || []).map(cloneUnknownObject),
      artifacts: (message.artifacts || []).map((artifact) => ({ ...artifact })),
    })),
    activities: (agentRun.activities || []).map((activity) => ({
      ...activity,
      call: {
        ...activity.call,
        normalizedInput: cloneUnknownObject(activity.call.normalizedInput),
      },
      result: activity.result ? {
        ...activity.result,
        operations: ((activity.result.operations || []) as unknown[]).map(cloneUnknownObject),
      } : null,
      waitResult: activity.waitResult ? { ...activity.waitResult } : null,
    })),
    citations: (agentRun.citations || []).map((citation) => ({ ...citation })),
  } as ProjectedAgentRun;
}

export function createChatViewStore({
  now = () => Date.now(),
  monotonicNow = () => performance.now(),
  scheduleFrame = (callback: () => void) => requestAnimationFrame(callback),
  cancelFrame = (frameId: number) => cancelAnimationFrame(frameId),
  onRenderTelemetry = () => {},
}: ChatViewStoreOptions = {}) {
  const agentRunsById = new Map<string, ProjectedAgentRun>();
  const messagesById = new Map<string, ChatMessage>();
  const messageIdsByAgentRunId = new Map<string, readonly string[]>();
  const agentRunListeners = new Map<string, Set<Listener>>();
  const activityDisclosures = new Map<string, ReadonlySet<string>>();
  const emptyDisclosures: ReadonlySet<string> = new Set();
  const listListeners = new Set<Listener>();
  const changeListeners = new Set<ChangeListener>();
  const pendingRenderTelemetry = new Map<string, PendingRenderTelemetry>();
  const renderTelemetryFrames = new Map<string, ScheduledRenderTelemetry>();
  const streamRenderRevisions = new Map<string, number>();
  let orderedAgentRunIds: readonly string[] = Object.freeze([]);
  let listSnapshot: ChatListSnapshot = Object.freeze({
    agentRunIds: orderedAgentRunIds,
    hasActiveAgentRun: false,
  });

  const cancelRenderTelemetry = (
    agentRunId: string,
    telemetry: PendingRenderTelemetry | null = null,
  ) => {
    const scheduled = renderTelemetryFrames.get(agentRunId);
    if (scheduled && (!telemetry || scheduled.telemetry === telemetry)) {
      cancelFrame(scheduled.frameId);
      renderTelemetryFrames.delete(agentRunId);
    }
    if (!telemetry || pendingRenderTelemetry.get(agentRunId) === telemetry) {
      pendingRenderTelemetry.delete(agentRunId);
    }
  };

  const clearRenderTelemetry = () => {
    [...renderTelemetryFrames.keys()].forEach((agentRunId) => cancelRenderTelemetry(agentRunId));
    pendingRenderTelemetry.clear();
  };

  const rebuildMessages = (agentRun: ProjectedAgentRun) => {
    const previousIds = messageIdsByAgentRunId.get(agentRun.id) || [];
    previousIds.forEach((messageId) => messagesById.delete(messageId));
    const ids: string[] = [];
    for (const message of agentRun.messages || []) {
      if (messagesById.has(message.messageId)) throw new Error(`duplicate messageId: ${message.messageId}`);
      ids.push(message.messageId);
      messagesById.set(message.messageId, message);
    }
    messageIdsByAgentRunId.set(agentRun.id, Object.freeze(ids));
  };

  const emitList = () => {
    listSnapshot = Object.freeze({
      agentRunIds: orderedAgentRunIds,
      hasActiveAgentRun: orderedAgentRunIds.some((agentRunId) => {
        const agentRun = agentRunsById.get(agentRunId);
        return agentRun ? isAgentRunActive(agentRun) : false;
      }),
    });
    listListeners.forEach((listener) => listener());
  };

  const emitAgentRun = (agentRunId: string, listMayHaveChanged = false) => {
    agentRunListeners.get(agentRunId)?.forEach((listener) => listener());
    changeListeners.forEach((listener) => listener(agentRunId));
    if (listMayHaveChanged) emitList();
  };

  const putAgentRun = (agentRun: ProjectedAgentRun) => {
    const cloned = cloneAgentRun(agentRun);
    agentRunsById.set(cloned.id, cloned);
    if (!streamRenderRevisions.has(cloned.id)) streamRenderRevisions.set(cloned.id, 0);
    rebuildMessages(cloned);
    return cloned;
  };

  const store: ChatViewStore = {
    getListSnapshot: () => listSnapshot,
    getAgentRunSnapshot: (agentRunId) => agentRunsById.get(agentRunId) || null,
    getMessageSnapshot: (messageId) => messagesById.get(messageId) || null,
    getMessageIdsForAgentRun: (agentRunId) => messageIdsByAgentRunId.get(agentRunId) || Object.freeze([]),
    getStreamRenderRevision: (agentRunId) => streamRenderRevisions.get(agentRunId) || 0,
    getActivityDisclosures: (agentRunId) => activityDisclosures.get(agentRunId) || emptyDisclosures,
    toggleActivityDisclosure(agentRunId, identity) {
      if (!agentRunsById.has(agentRunId)) throw new Error(`unknown agentRunId: ${agentRunId}`);
      const expanded = new Set(activityDisclosures.get(agentRunId));
      if (expanded.has(identity)) expanded.delete(identity);
      else expanded.add(identity);
      if (expanded.size) activityDisclosures.set(agentRunId, expanded);
      else activityDisclosures.delete(agentRunId);
      agentRunListeners.get(agentRunId)?.forEach((listener) => listener());
    },
    subscribeList(listener) {
      listListeners.add(listener);
      return () => listListeners.delete(listener);
    },
    subscribeAgentRun(agentRunId, listener) {
      if (!agentRunId) return () => {};
      let listeners = agentRunListeners.get(agentRunId);
      if (!listeners) {
        listeners = new Set();
        agentRunListeners.set(agentRunId, listeners);
      }
      listeners.add(listener);
      return () => {
        const listeners = agentRunListeners.get(agentRunId);
        listeners?.delete(listener);
        if (listeners?.size === 0) agentRunListeners.delete(agentRunId);
      };
    },
    subscribeChanges(listener) {
      changeListeners.add(listener);
      return () => changeListeners.delete(listener);
    },
    clear() {
      clearRenderTelemetry();
      activityDisclosures.clear();
      const previousIds = orderedAgentRunIds;
      agentRunsById.clear();
      messagesById.clear();
      messageIdsByAgentRunId.clear();
      streamRenderRevisions.clear();
      orderedAgentRunIds = Object.freeze([]);
      previousIds.forEach((agentRunId) => agentRunListeners.get(agentRunId)?.forEach((listener) => listener()));
      emitList();
    },
    replaceAll(agentRuns) {
      clearRenderTelemetry();
      const nextIds = [];
      const seen = new Set();
      agentRunsById.clear();
      messagesById.clear();
      messageIdsByAgentRunId.clear();
      streamRenderRevisions.clear();
      for (const agentRun of agentRuns) {
        if (seen.has(agentRun.id)) throw new Error(`duplicate agentRunId: ${agentRun.id}`);
        seen.add(agentRun.id);
        nextIds.push(agentRun.id);
        putAgentRun(agentRun);
      }
      orderedAgentRunIds = Object.freeze(nextIds);
      for (const agentRunId of activityDisclosures.keys()) {
        if (!seen.has(agentRunId)) activityDisclosures.delete(agentRunId);
      }
      emitList();
      nextIds.forEach((agentRunId) => emitAgentRun(agentRunId));
    },
    prependAgentRuns(agentRuns) {
      if (!agentRuns.length) return;
      const existing = new Set(orderedAgentRunIds);
      const newIds: string[] = [];
      for (const agentRun of agentRuns) {
        if (existing.has(agentRun.id) || newIds.includes(agentRun.id)) throw new Error(`history page overlaps agentRunId: ${agentRun.id}`);
        newIds.push(agentRun.id);
        putAgentRun(agentRun);
      }
      orderedAgentRunIds = Object.freeze([...newIds, ...orderedAgentRunIds]);
      emitList();
    },
    appendAgentRun(agentRun) {
      if (agentRunsById.has(agentRun.id)) throw new Error(`duplicate agentRunId: ${agentRun.id}`);
      putAgentRun(agentRun);
      orderedAgentRunIds = Object.freeze([...orderedAgentRunIds, agentRun.id]);
      emitList();
      emitAgentRun(agentRun.id);
    },
    replaceAgentRun(agentRun) {
      const previous = agentRunsById.get(agentRun.id);
      if (!previous) throw new Error(`unknown agentRunId: ${agentRun.id}`);
      if (previous.sessionId === agentRun.sessionId && previous.citationSequence > agentRun.citationSequence) {
        agentRun = { ...agentRun, citations: previous.citations, citationSequence: previous.citationSequence };
      }
      putAgentRun(agentRun);
      emitAgentRun(agentRun.id, isAgentRunActive(previous) !== isAgentRunActive(agentRun));
    },
    applyCitationSnapshot(snapshot) {
      const previous = agentRunsById.get(snapshot.agentRunId);
      if (!previous || previous.sessionId !== snapshot.sessionId || snapshot.throughSequence < previous.citationSequence) return;
      putAgentRun({ ...previous, citations: snapshot.citations, citationSequence: snapshot.throughSequence });
      emitAgentRun(previous.id);
    },
    replaceAgentRunId(previousAgentRunId, agentRun) {
      const index = orderedAgentRunIds.indexOf(previousAgentRunId);
      if (index < 0) throw new Error(`unknown pending agentRunId: ${previousAgentRunId}`);
      if (previousAgentRunId !== agentRun.id && agentRunsById.has(agentRun.id)) throw new Error(`duplicate agentRunId: ${agentRun.id}`);
      const previousMessageIds = messageIdsByAgentRunId.get(previousAgentRunId) || [];
      previousMessageIds.forEach((messageId) => messagesById.delete(messageId));
      messageIdsByAgentRunId.delete(previousAgentRunId);
      agentRunsById.delete(previousAgentRunId);
      activityDisclosures.delete(previousAgentRunId);
      const streamRenderRevision = streamRenderRevisions.get(previousAgentRunId) || 0;
      streamRenderRevisions.delete(previousAgentRunId);
      putAgentRun(agentRun);
      streamRenderRevisions.set(agentRun.id, streamRenderRevision);
      orderedAgentRunIds = Object.freeze(orderedAgentRunIds.map((agentRunId) => agentRunId === previousAgentRunId ? agentRun.id : agentRunId));
      agentRunListeners.get(previousAgentRunId)?.forEach((listener) => listener());
      emitList();
      emitAgentRun(agentRun.id);
    },
    updateConnection(agentRunId, connection) {
      const agentRun = agentRunsById.get(agentRunId);
      if (!agentRun) throw new Error(`unknown agentRunId: ${agentRunId}`);
      const wasActive = isAgentRunActive(agentRun);
      const next = { ...agentRun, connection };
      if (displayPhaseKey(next) !== displayPhaseKey(agentRun)) next.phaseStartedAtMs = now();
      agentRunsById.set(agentRunId, next);
      emitAgentRun(agentRunId, wasActive !== isAgentRunActive(next));
    },
    rejectPendingAgentRun(agentRunId) {
      if (!agentRunId.startsWith("pending:")) throw new Error("only a pending AgentRun may be rejected locally");
      const agentRun = agentRunsById.get(agentRunId);
      if (!agentRun) return;
      if (!isAgentRunActive(agentRun)) return;
      const next: ProjectedAgentRun = {
        ...agentRun,
        status: "failed",
        connection: "failed",
        finishedAtMs: now(),
      };
      agentRunsById.set(agentRunId, next);
      emitAgentRun(agentRunId, true);
    },
    markAgentRunProjectionError(agentRunId) {
      const agentRun = agentRunsById.get(agentRunId);
      if (!agentRun) return;
      agentRunsById.set(agentRunId, {
        ...agentRun,
        live: null,
        projectionError: "session_projection_invalid",
      });
      emitAgentRun(agentRunId);
    },
    beginStreamTelemetry(entry, acceptedOrdinal) {
      return Object.freeze({
        entry,
        acceptedAt: monotonicNow(),
        acceptedOrdinal,
        agentRunId: entry.item.agentRunId,
        streamItemKind: entry.item.kind,
      });
    },
    applyStreamEntries(entries, telemetry = []) {
      if (telemetry.length && telemetry.length !== entries.length) throw new Error("stream telemetry batch length mismatch");
      const changed = new Map<string, ProjectedAgentRun>();
      const activeChanges = new Set<string>();
      for (const entry of entries) {
        const agentRunId = entry.item.agentRunId;
        const current = changed.get(agentRunId) || agentRunsById.get(agentRunId);
        if (!current) throw new Error(`session stream references unknown agentRunId: ${agentRunId}`);
        const wasActive = isAgentRunActive(current);
        let next = applyStreamEntry(current, entry);
        if (displayPhaseKey(next) === displayPhaseKey(current)) {
          next = { ...next, phaseStartedAtMs: current.phaseStartedAtMs };
        }
        if (wasActive !== isAgentRunActive(next)) activeChanges.add(agentRunId);
        changed.set(agentRunId, next);
      }
      changed.forEach((_, agentRunId) => {
        streamRenderRevisions.set(agentRunId, (streamRenderRevisions.get(agentRunId) || 0) + 1);
      });
      if (telemetry.length) {
        const reducerAppliedAt = monotonicNow();
        telemetry.forEach((item, index) => {
          if (!item || item.entry !== entries[index]) throw new Error("stream telemetry entry mismatch");
          if (!changed.has(item.agentRunId)) throw new Error("stream telemetry AgentRun mismatch");
          cancelRenderTelemetry(item.agentRunId);
          pendingRenderTelemetry.set(item.agentRunId, {
            ...item,
            renderRevision: streamRenderRevisions.get(item.agentRunId) ?? 0,
            reducerAppliedAt,
          });
        });
      }
      changed.forEach((agentRun, agentRunId) => {
        agentRunsById.set(agentRunId, agentRun);
        rebuildMessages(agentRun);
        emitAgentRun(agentRunId, activeChanges.has(agentRunId));
      });
    },
    markDomCommit(agentRunId, renderRevision) {
      const telemetry = pendingRenderTelemetry.get(agentRunId);
      if (!telemetry || telemetry.renderRevision !== renderRevision) return undefined;
      const domCommitAt = monotonicNow();
      telemetry.domCommitAt = domCommitAt;
      const schedule = (callback: () => void) => {
        const frameId = scheduleFrame(callback);
        renderTelemetryFrames.set(agentRunId, { frameId, telemetry });
      };
      schedule(() => {
        if (pendingRenderTelemetry.get(agentRunId) !== telemetry) return;
        schedule(() => {
          if (pendingRenderTelemetry.get(agentRunId) !== telemetry) return;
          const domPaintBoundaryAt = monotonicNow();
          pendingRenderTelemetry.delete(agentRunId);
          renderTelemetryFrames.delete(agentRunId);
          const metric: RenderTelemetryMetric = Object.freeze({
            schema: "workspace.chat_render_telemetry.v1",
            agentRunId,
            acceptedOrdinal: telemetry.acceptedOrdinal,
            streamItemKind: telemetry.streamItemKind,
            acceptedAt: telemetry.acceptedAt,
            reducerAppliedAt: telemetry.reducerAppliedAt,
            domCommitAt,
            domPaintBoundaryAt,
            acceptedToReducerMs: telemetry.reducerAppliedAt - telemetry.acceptedAt,
            reducerToDomCommitMs: domCommitAt - telemetry.reducerAppliedAt,
            domCommitToPaintBoundaryMs: domPaintBoundaryAt - domCommitAt,
            acceptedToDomPaintBoundaryMs: domPaintBoundaryAt - telemetry.acceptedAt,
          });
          try {
            onRenderTelemetry(metric);
          } catch {
            // Telemetry must never break chat rendering.
          }
        });
      });
      return () => cancelRenderTelemetry(agentRunId, telemetry);
    },
  };
  return store;
}
