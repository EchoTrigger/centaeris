import { useCallback, useEffect, useRef, useState } from "react";
import { recoverQueuedPromptAfterStop } from "./chatAreaModel";

const normalizeSessionId = (sessionId: string): string => sessionId.trim();

export const useQueuedPromptLifecycle = (currentSessionId: string) => {
  const [queuedPrompt, setQueuedPrompt] = useState("");
  const queuedPromptRef = useRef("");
  const queuedPromptSessionIdRef = useRef("");

  const clearQueuedPrompt = useCallback(() => {
    queuedPromptRef.current = "";
    queuedPromptSessionIdRef.current = "";
    setQueuedPrompt("");
  }, []);

  const queuePrompt = useCallback((value: string, sessionId: string) => {
    const normalizedSessionId = normalizeSessionId(sessionId);
    queuedPromptRef.current = value;
    queuedPromptSessionIdRef.current = value.trim()
      ? normalizedSessionId
      : "";
    setQueuedPrompt(value);
  }, []);

  const takeQueuedPromptForSession = useCallback(
    (sessionId: string): string => {
      const normalizedSessionId = normalizeSessionId(sessionId);
      if (queuedPromptSessionIdRef.current !== normalizedSessionId) {
        return "";
      }
      const prompt = queuedPromptRef.current.trim();
      clearQueuedPrompt();
      return prompt;
    },
    [clearQueuedPrompt],
  );

  const takeQueuedPromptForEditing = useCallback(
    (sessionId: string): string => {
      const normalizedSessionId = normalizeSessionId(sessionId);
      if (queuedPromptSessionIdRef.current !== normalizedSessionId) {
        return "";
      }
      const prompt = queuedPromptRef.current;
      clearQueuedPrompt();
      return prompt;
    },
    [clearQueuedPrompt],
  );

  const recoverQueuedPromptForStop = useCallback(
    (sessionId: string, currentDraft: string): string => {
      const normalizedSessionId = normalizeSessionId(sessionId);
      const prompt =
        queuedPromptSessionIdRef.current === normalizedSessionId
          ? queuedPromptRef.current
          : "";
      clearQueuedPrompt();
      return recoverQueuedPromptAfterStop(prompt, currentDraft);
    },
    [clearQueuedPrompt],
  );

  useEffect(() => {
    if (
      queuedPromptSessionIdRef.current &&
      queuedPromptSessionIdRef.current !== normalizeSessionId(currentSessionId)
    ) {
      clearQueuedPrompt();
    }
  }, [clearQueuedPrompt, currentSessionId]);

  return {
    queuedPrompt,
    queuePrompt,
    takeQueuedPromptForSession,
    takeQueuedPromptForEditing,
    recoverQueuedPromptForStop,
  };
};
