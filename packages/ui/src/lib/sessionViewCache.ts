import type { AgentStreamPayload } from "./chatBridge";

export const SESSION_VIEW_CACHE_SCHEMA = "session_view_cache_v1" as const;

export type SessionReplayCursors = Record<string, number>;

export type SessionViewCacheEntry<TSnapshot> = {
  schema: typeof SESSION_VIEW_CACHE_SCHEMA;
  sessionId: string;
  snapshot: TSnapshot;
  replayCursorsByAgentRunId: SessionReplayCursors;
  verifiedReplayAgentRunIds: string[];
  hydratedAtMs: number;
  updatedAtMs: number;
  dirty: boolean;
};

export type SessionViewCacheReplayDecision =
  | { kind: "incremental" }
  | { kind: "fullReplay"; reason: string };

type SessionViewCacheWrite<TSnapshot> = {
  sessionId: string;
  snapshot: TSnapshot;
  replayCursorsByAgentRunId?: SessionReplayCursors;
  verifiedReplayAgentRunIds?: readonly string[];
  hydratedAtMs?: number;
  dirty?: boolean;
};

type SessionViewCacheStoreOptions = {
  maxEntries?: number;
};

const DEFAULT_MAX_SESSION_VIEW_CACHE_ENTRIES = 12;

const normalizeAgentRunId = (value: unknown): string =>
  typeof value === "string" ? value.trim() : "";

export const getAgentStreamPayloadAgentRunId = (
  payload: AgentStreamPayload,
): string =>
  payload.type === "session_event"
    ? normalizeAgentRunId(payload.agentRunId)
    : "";

export const getAgentStreamPayloadCursor = (
  payload: AgentStreamPayload,
): number | null => {
  if (
    typeof payload.cursor === "number" &&
    Number.isInteger(payload.cursor) &&
    payload.cursor >= 0
  ) {
    return payload.cursor;
  }
  return null;
};

export const deriveReplayCursorPatch = (
  payloads: readonly AgentStreamPayload[],
): SessionReplayCursors => {
  const patch: SessionReplayCursors = {};
  for (const payload of payloads) {
    const agentRunId = getAgentStreamPayloadAgentRunId(payload);
    const cursor = getAgentStreamPayloadCursor(payload);
    if (!agentRunId || cursor === null) {
      continue;
    }
    const nextCursor = cursor + 1;
    patch[agentRunId] = Math.max(patch[agentRunId] ?? 0, nextCursor);
  }
  return patch;
};

export const mergeReplayCursors = (
  current: SessionReplayCursors,
  patch: SessionReplayCursors,
): SessionReplayCursors => {
  const next: SessionReplayCursors = { ...current };
  for (const [agentRunId, cursor] of Object.entries(patch)) {
    const normalizedAgentRunId = normalizeAgentRunId(agentRunId);
    if (!normalizedAgentRunId) {
      throw new Error("session view cache replay cursor agentRunId is required");
    }
    if (!Number.isInteger(cursor) || cursor < 0) {
      throw new Error(
        `session view cache replay cursor is invalid for ${normalizedAgentRunId}: ${cursor}`,
      );
    }
    next[normalizedAgentRunId] = Math.max(next[normalizedAgentRunId] ?? 0, cursor);
  }
  return next;
};

const normalizeReplayCursors = (
  cursors: SessionReplayCursors | undefined,
): SessionReplayCursors => mergeReplayCursors({}, cursors ?? {});

const normalizeReplayAgentRunIds = (agentRunIds: readonly unknown[] | undefined): string[] =>
  Array.from(new Set((agentRunIds ?? []).map(normalizeAgentRunId).filter(Boolean)));

export const decideSessionViewCacheReplay = (input: {
  durableMessageIds: readonly string[];
  durableStreamAgentRunIds: readonly string[];
  cachedMessageIds: readonly string[];
  cachedReplayCursorsByAgentRunId: SessionReplayCursors;
  cachedVerifiedReplayAgentRunIds?: readonly string[];
}): SessionViewCacheReplayDecision => {
  const cachedMessageIds = new Set(
    input.cachedMessageIds.map(normalizeAgentRunId).filter(Boolean),
  );
  for (const messageId of input.durableMessageIds) {
    const normalizedMessageId = normalizeAgentRunId(messageId);
    if (!normalizedMessageId) {
      continue;
    }
    if (!cachedMessageIds.has(normalizedMessageId)) {
      return {
        kind: "fullReplay",
        reason: `missing_durable_message:${normalizedMessageId}`,
      };
    }
  }

  const cachedReplayAgentRunIds = new Set(
    Object.keys(input.cachedReplayCursorsByAgentRunId)
      .map(normalizeAgentRunId)
      .filter(Boolean),
  );
  const durableStreamAgentRunIds = new Set(
    input.durableStreamAgentRunIds.map(normalizeAgentRunId).filter(Boolean),
  );
  for (const agentRunId of cachedReplayAgentRunIds) {
    if (!durableStreamAgentRunIds.has(agentRunId)) {
      return {
        kind: "fullReplay",
        reason: `stale_replay_cursor:${agentRunId}`,
      };
    }
  }
  const cachedVerifiedReplayAgentRunIds = new Set(
    normalizeReplayAgentRunIds(input.cachedVerifiedReplayAgentRunIds),
  );
  for (const agentRunId of input.durableStreamAgentRunIds) {
    const normalizedAgentRunId = normalizeAgentRunId(agentRunId);
    if (!normalizedAgentRunId) {
      continue;
    }
    if (!cachedReplayAgentRunIds.has(normalizedAgentRunId)) {
      return {
        kind: "fullReplay",
        reason: `missing_replay_cursor:${normalizedAgentRunId}`,
      };
    }
    if (!cachedVerifiedReplayAgentRunIds.has(normalizedAgentRunId)) {
      return {
        kind: "fullReplay",
        reason: `unverified_replay_projection:${normalizedAgentRunId}`,
      };
    }
  }

  return { kind: "incremental" };
};

export const createSessionViewCacheStore = <TSnapshot>(
  options: SessionViewCacheStoreOptions = {},
) => {
  const maxEntries = Math.max(
    1,
    Math.floor(options.maxEntries ?? DEFAULT_MAX_SESSION_VIEW_CACHE_ENTRIES),
  );
  const entries = new Map<string, SessionViewCacheEntry<TSnapshot>>();

  const normalizeSessionId = (sessionId: string): string => {
    const normalized = sessionId.trim();
    if (!normalized) {
      throw new Error("session view cache sessionId is required");
    }
    return normalized;
  };

  const prune = (): void => {
    while (entries.size > maxEntries) {
      const oldest = entries.keys().next().value;
      if (!oldest) {
        return;
      }
      entries.delete(oldest);
    }
  };

  const touch = (entry: SessionViewCacheEntry<TSnapshot>): void => {
    entries.delete(entry.sessionId);
    entries.set(entry.sessionId, entry);
  };

  return {
    get(sessionId: string): SessionViewCacheEntry<TSnapshot> | null {
      const normalized = sessionId.trim();
      if (!normalized) {
        return null;
      }
      const entry = entries.get(normalized);
      if (!entry) {
        return null;
      }
      touch(entry);
      return entry;
    },

    write(input: SessionViewCacheWrite<TSnapshot>): SessionViewCacheEntry<TSnapshot> {
      const sessionId = normalizeSessionId(input.sessionId);
      const existing = entries.get(sessionId);
      const now = Date.now();
      const entry: SessionViewCacheEntry<TSnapshot> = {
        schema: SESSION_VIEW_CACHE_SCHEMA,
        sessionId,
        snapshot: input.snapshot,
        replayCursorsByAgentRunId: normalizeReplayCursors(
          input.replayCursorsByAgentRunId,
        ),
        verifiedReplayAgentRunIds: normalizeReplayAgentRunIds(
          input.verifiedReplayAgentRunIds ?? existing?.verifiedReplayAgentRunIds,
        ),
        hydratedAtMs: input.hydratedAtMs ?? existing?.hydratedAtMs ?? now,
        updatedAtMs: now,
        dirty: input.dirty ?? false,
      };
      touch(entry);
      prune();
      return entry;
    },

    patchReplayCursors(
      sessionId: string,
      patch: SessionReplayCursors,
    ): SessionViewCacheEntry<TSnapshot> | null {
      const normalized = sessionId.trim();
      if (!normalized) {
        return null;
      }
      const existing = entries.get(normalized);
      if (!existing) {
        return null;
      }
      const entry: SessionViewCacheEntry<TSnapshot> = {
        ...existing,
        replayCursorsByAgentRunId: mergeReplayCursors(
          existing.replayCursorsByAgentRunId,
          patch,
        ),
        verifiedReplayAgentRunIds: existing.verifiedReplayAgentRunIds,
        updatedAtMs: Date.now(),
        dirty: true,
      };
      touch(entry);
      return entry;
    },

    delete(sessionId: string): void {
      const normalized = sessionId.trim();
      if (normalized) {
        entries.delete(normalized);
      }
    },

    clear(): void {
      entries.clear();
    },

    size(): number {
      return entries.size;
    },
  };
};
