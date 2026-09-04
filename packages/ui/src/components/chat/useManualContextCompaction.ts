import { useCallback, useRef, useState } from "react";
import { compactAgentContext } from "../../lib/chatBridge";
import { formatExecutionError } from "./chatRuntimeCore";

type ActiveCompaction = {
  requestId: number;
  sessionId: string;
};

type UseManualContextCompactionOptions = {
  sessionId: string;
  isStreaming: boolean;
  refreshContextUsage: (sessionId: string) => Promise<void>;
  onError: (message: string) => void;
};

export const useManualContextCompaction = ({
  sessionId,
  isStreaming,
  refreshContextUsage,
  onError,
}: UseManualContextCompactionOptions) => {
  const normalizedSessionId = sessionId.trim();
  const currentSessionIdRef = useRef(normalizedSessionId);
  currentSessionIdRef.current = normalizedSessionId;
  const nextRequestIdRef = useRef(0);
  const activeCompactionRef = useRef<ActiveCompaction | null>(null);
  const [activeCompaction, setActiveCompaction] =
    useState<ActiveCompaction | null>(null);

  const compactContext = useCallback(async () => {
    const targetSessionId = normalizedSessionId;
    if (!targetSessionId || isStreaming) {
      return;
    }
    if (activeCompactionRef.current?.sessionId === targetSessionId) {
      return;
    }

    const requestId = nextRequestIdRef.current + 1;
    nextRequestIdRef.current = requestId;
    const request = { requestId, sessionId: targetSessionId };
    activeCompactionRef.current = request;
    setActiveCompaction(request);
    onError("");

    const isCurrentRequest = () =>
      currentSessionIdRef.current === targetSessionId &&
      activeCompactionRef.current?.requestId === requestId;

    try {
      const result = await compactAgentContext(targetSessionId);
      if (!isCurrentRequest()) {
        return;
      }
      if (!result.compacted) {
        onError("Not enough conversation history to compact.");
      }
      await refreshContextUsage(targetSessionId);
    } catch (error) {
      if (isCurrentRequest()) {
        onError(`Compaction failed: ${formatExecutionError(error)}`);
      }
    } finally {
      if (activeCompactionRef.current?.requestId === requestId) {
        activeCompactionRef.current = null;
      }
      setActiveCompaction((current) =>
        current?.requestId === requestId ? null : current,
      );
    }
  }, [isStreaming, normalizedSessionId, onError, refreshContextUsage]);

  return {
    isManualCompacting:
      activeCompaction?.sessionId === normalizedSessionId,
    compactContext,
  };
};
