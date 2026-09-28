import { useCallback, useEffect, useRef, useState } from "react";
import type { SessionViewCacheEntry } from "../../lib/sessionViewCache";
import { waitForNextPaint } from "./chatAreaModel";
import {
  formatExecutionError,
  sessionViewCacheStore,
} from "./chatRuntimeCore";
import { buildSessionHydrationSnapshot } from "./chatRuntimeModel";
import type {
  SessionHydrationSnapshot,
  SessionViewSnapshot,
} from "./types";

export type SessionHydrationControl = {
  isLatest: () => boolean;
  onStage: (stage: string) => void;
};

export type SessionHydrationPlan =
  | { kind: "none" }
  | { kind: "preserved" }
  | {
      kind: "cached";
      entry: SessionViewCacheEntry<SessionViewSnapshot>;
    }
  | { kind: "fresh" };

type UseSessionHydrationOptions = {
  currentSessionId: string;
  prepare: (sessionId: string) => SessionHydrationPlan;
  applySnapshot: (
    snapshot: SessionHydrationSnapshot,
    sessionId: string,
  ) => void;
  refreshCachedSession: (
    sessionId: string,
    entry: SessionViewCacheEntry<SessionViewSnapshot>,
    control: SessionHydrationControl,
  ) => Promise<void>;
  onError: (message: string) => void;
};

type SessionHydrationState = {
  isHydratingSession: boolean;
  hydrationStage: string;
  isSyncingSession: boolean;
  syncError: string;
  reportSyncError: (message: string) => void;
  retrySessionSync: () => void;
};

export const useSessionHydration = ({
  currentSessionId,
  prepare,
  applySnapshot,
  refreshCachedSession,
  onError,
}: UseSessionHydrationOptions): SessionHydrationState => {
  const requestIdRef = useRef(0);
  const [isHydratingSession, setIsHydratingSession] = useState(false);
  const [hydrationStage, setHydrationStage] = useState("");
  const [isSyncingSession, setIsSyncingSession] = useState(false);
  const [syncError, reportSyncError] = useState("");
  const [retryRevision, setRetryRevision] = useState(0);
  const retrySessionSync = useCallback(() => setRetryRevision((value) => value + 1), []);
  const callbacks = useRef({ prepare, applySnapshot, refreshCachedSession, onError });
  callbacks.current = { prepare, applySnapshot, refreshCachedSession, onError };

  useEffect(() => {
    // Explicit retry is an identity-preserving synchronization request.
    void retryRevision;
    reportSyncError("");
    setIsSyncingSession(false);
    const plan = callbacks.current.prepare(currentSessionId);
    if (plan.kind === "preserved") {
      setIsHydratingSession(false);
      setHydrationStage("");
      return undefined;
    }

    requestIdRef.current += 1;
    if (plan.kind === "none") {
      setIsHydratingSession(false);
      setHydrationStage("");
      return undefined;
    }

    setIsSyncingSession(true);
    let cancelled = false;
    let retryTimer: ReturnType<typeof setTimeout> | undefined;
    let finishWait: (() => void) | undefined;
    const requestId = requestIdRef.current;
    const isLatest = () =>
      !cancelled && requestIdRef.current === requestId;
    const onStage = (stage: string) => {
      if (isLatest()) {
        setHydrationStage(stage);
      }
    };

    if (plan.kind === "cached") {
      setIsHydratingSession(false);
      setHydrationStage("");
    } else {
      setIsHydratingSession(true);
      setHydrationStage("fetchProjection");
    }

    const hydrateSession = async () => {
      try {
        if (plan.kind === "cached") {
          for (let attempt = 0; ; attempt += 1) {
            try {
              await callbacks.current.refreshCachedSession(currentSessionId, plan.entry, { isLatest, onStage });
              break;
            } catch (error) {
              if (!isLatest()) return;
              // Retry reads only, once. Contract/projection failures remain visible.
              if (attempt > 0 || !/timed? ?out|disconnected|ECONNRESET|EPIPE|temporarily unavailable/i.test(String(error))) throw error;
              await new Promise<void>((resolve) => { finishWait = resolve; retryTimer = setTimeout(resolve, 500); });
              if (!isLatest()) return;
            }
          }
        } else {
          const snapshot = await buildSessionHydrationSnapshot(
            currentSessionId,
            {
              isCancelled: () => !isLatest(),
              yieldToUi: waitForNextPaint,
              onStage,
            },
          );
          if (!isLatest()) {
            return;
          }
          onStage("applySnapshot");
          callbacks.current.applySnapshot(snapshot, currentSessionId);
        }
        if (!isLatest()) {
          return;
        }
        setIsSyncingSession(false);
        setIsHydratingSession(false);
        setHydrationStage("");
      } catch (error) {
        if (!isLatest()) {
          return;
        }
        setIsSyncingSession(false);
        setIsHydratingSession(false);
        setHydrationStage("");
        if (plan.kind === "cached") {
          reportSyncError(formatExecutionError(error));
        } else {
          sessionViewCacheStore.delete(currentSessionId);
          callbacks.current.onError(formatExecutionError(error));
        }
      }
    };

    void hydrateSession();
    return () => {
      cancelled = true;
      clearTimeout(retryTimer);
      finishWait?.();
      if (requestIdRef.current === requestId) {
        setIsSyncingSession(false);
        setIsHydratingSession(false);
        setHydrationStage("");
      }
    };
  }, [currentSessionId, retryRevision]);

  return { isHydratingSession, isSyncingSession, hydrationStage, syncError, reportSyncError, retrySessionSync };
};
