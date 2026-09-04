import { useCallback, useRef, useState } from "react";
import {
  getAgentContextUsage,
  type AgentContextUsageSummary,
} from "../../lib/chatBridge";

const normalizeSessionId = (sessionId: string): string => sessionId.trim();

const shouldKeepCurrentUsage = (
  current: AgentContextUsageSummary,
  next: AgentContextUsageSummary,
): boolean => {
  const currentUpdatedAt = current.updatedAt;
  const nextUpdatedAt = next.updatedAt;
  return (
    typeof currentUpdatedAt === "number" &&
    (typeof nextUpdatedAt !== "number" || currentUpdatedAt > nextUpdatedAt)
  );
};

export const useAgentContextUsage = (currentSessionId: string) => {
  const normalizedCurrentSessionId = normalizeSessionId(currentSessionId);
  const currentSessionIdRef = useRef(normalizedCurrentSessionId);
  currentSessionIdRef.current = normalizedCurrentSessionId;
  const [storedContextUsage, setStoredContextUsage] =
    useState<AgentContextUsageSummary | null>(null);

  const applyContextUsage = useCallback(
    (sessionId: string, next: AgentContextUsageSummary | null) => {
      const normalized = normalizeSessionId(sessionId);
      if (!normalized || currentSessionIdRef.current !== normalized) {
        return;
      }
      setStoredContextUsage((current) => {
        const visibleCurrent =
          current?.sessionId === normalized ? current : null;
        if (!next) {
          return visibleCurrent;
        }
        if (normalizeSessionId(next.sessionId) !== normalized) {
          return visibleCurrent;
        }
        if (visibleCurrent && shouldKeepCurrentUsage(visibleCurrent, next)) {
          return visibleCurrent;
        }
        return next;
      });
    },
    [],
  );

  const resetContextUsage = useCallback((sessionId: string) => {
    if (currentSessionIdRef.current === normalizeSessionId(sessionId)) {
      setStoredContextUsage(null);
    }
  }, []);

  const markContextCompacting = useCallback(
    (sessionId: string, isCompacting: boolean) => {
      const normalized = normalizeSessionId(sessionId);
      if (!normalized || currentSessionIdRef.current !== normalized) {
        return;
      }
      setStoredContextUsage((current) =>
        current?.sessionId === normalized
          ? { ...current, isCompacting }
          : current,
      );
    },
    [],
  );

  const refreshContextUsage = useCallback(
    async (sessionId: string) => {
      const normalized = normalizeSessionId(sessionId);
      if (!normalized) {
        resetContextUsage("");
        return;
      }
      try {
        const next = await getAgentContextUsage(normalized);
        applyContextUsage(normalized, next);
      } catch {
        // A transient state read must not erase the last canonical request boundary.
      }
    },
    [applyContextUsage, resetContextUsage],
  );

  const contextUsage =
    storedContextUsage?.sessionId === normalizedCurrentSessionId
      ? storedContextUsage
      : null;

  return {
    contextUsage,
    applyContextUsage,
    resetContextUsage,
    markContextCompacting,
    refreshContextUsage,
  };
};
