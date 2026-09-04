import { useCallback, useRef, useState, type RefObject } from "react";
import type { ActiveStreamState } from "./types";

type StreamIdentity = Pick<
  ActiveStreamState,
  "agentRunId" | "assistantMessageId"
>;

export type AgentStreamConnection = {
  getActiveStream: () => ActiveStreamState | null;
  isActiveStream: (identity: StreamIdentity) => boolean;
  isStreaming: boolean;
  setIsStreaming: (isStreaming: boolean) => void;
  setActiveStream: (stream: ActiveStreamState) => void;
  markStreamOpen: (identity: StreamIdentity) => void;
  closeActiveStream: () => void;
  closeStreamForMessage: (assistantMessageId: string) => void;
};

export const useAgentStreamConnection = (
  beforeCloseRef: RefObject<() => void>,
): AgentStreamConnection => {
  const [isStreaming, setIsStreaming] = useState(false);
  const activeStreamRef = useRef<ActiveStreamState | null>(null);
  const getActiveStream = useCallback(
    (): ActiveStreamState | null => activeStreamRef.current,
    [],
  );
  const isActiveStream = useCallback((identity: StreamIdentity): boolean => {
    const active = activeStreamRef.current;
    return (
      active?.assistantMessageId === identity.assistantMessageId &&
      active.agentRunId === identity.agentRunId
    );
  }, []);

  const closeActiveStream = useCallback(() => {
    const active = activeStreamRef.current;
    if (!active) {
      return;
    }
    beforeCloseRef.current();
    active.close();
    activeStreamRef.current = null;
    setIsStreaming(false);
  }, [beforeCloseRef]);

  const closeStreamForMessage = useCallback(
    (assistantMessageId: string) => {
      const active = activeStreamRef.current;
      if (!active || active.assistantMessageId !== assistantMessageId) {
        return;
      }
      beforeCloseRef.current();
      closeActiveStream();
    },
    [beforeCloseRef, closeActiveStream],
  );

  const setActiveStream = useCallback((stream: ActiveStreamState) => {
    activeStreamRef.current = stream;
  }, []);

  const markStreamOpen = useCallback((identity: StreamIdentity) => {
    const active = activeStreamRef.current;
    if (
      active?.assistantMessageId === identity.assistantMessageId &&
      active.agentRunId === identity.agentRunId
    ) {
      setIsStreaming(true);
    }
  }, []);

  return {
    getActiveStream,
    isActiveStream,
    isStreaming,
    setIsStreaming,
    setActiveStream,
    markStreamOpen,
    closeActiveStream,
    closeStreamForMessage,
  };
};
