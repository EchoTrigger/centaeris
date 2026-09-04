import { useEffect, useRef, useState } from "react";
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

  useEffect(() => {
    const plan = prepare(currentSessionId);
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

    let cancelled = false;
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
          await refreshCachedSession(currentSessionId, plan.entry, {
            isLatest,
            onStage,
          });
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
          applySnapshot(snapshot, currentSessionId);
        }
        if (!isLatest()) {
          return;
        }
        setIsHydratingSession(false);
        setHydrationStage("");
      } catch (error) {
        if (!isLatest()) {
          return;
        }
        sessionViewCacheStore.delete(currentSessionId);
        setIsHydratingSession(false);
        setHydrationStage("");
        onError(formatExecutionError(error));
      }
    };

    void hydrateSession();
    return () => {
      cancelled = true;
      if (requestIdRef.current === requestId) {
        setIsHydratingSession(false);
        setHydrationStage("");
      }
    };
  }, [
    applySnapshot,
    currentSessionId,
    onError,
    prepare,
    refreshCachedSession,
  ]);

  return { isHydratingSession, hydrationStage };
};
