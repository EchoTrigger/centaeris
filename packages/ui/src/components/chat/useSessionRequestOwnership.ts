import { useCallback, useEffect, useRef } from "react";

type SessionRequestOwner = {
  sessionId: string;
  epoch: number;
};

type SessionViewIdentity = {
  sessionId: string;
  epoch: number;
};

const normalizeSessionId = (sessionId: string): string => sessionId.trim();

export const useSessionRequestOwnership = (currentSessionId: string) => {
  const normalizedCurrentSessionId = normalizeSessionId(currentSessionId);
  const mountedRef = useRef(true);
  const viewIdentityRef = useRef<SessionViewIdentity>({
    sessionId: normalizedCurrentSessionId,
    epoch: 0,
  });

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    const current = viewIdentityRef.current;
    if (current.sessionId === normalizedCurrentSessionId) {
      return;
    }
    viewIdentityRef.current = {
      sessionId: normalizedCurrentSessionId,
      epoch: current.epoch + 1,
    };
  }, [normalizedCurrentSessionId]);

  const adoptSession = useCallback((sessionId: string) => {
    const normalized = normalizeSessionId(sessionId);
    if (!normalized) {
      throw new Error("cannot adopt an empty session id");
    }
    const current = viewIdentityRef.current;
    if (current.sessionId === normalized) {
      return;
    }
    viewIdentityRef.current = {
      sessionId: normalized,
      epoch: current.epoch + 1,
    };
  }, []);

  const captureSessionRequest = useCallback(
    (sessionId: string): SessionRequestOwner => ({
      sessionId: normalizeSessionId(sessionId),
      epoch: viewIdentityRef.current.epoch,
    }),
    [],
  );

  const ownsSessionRequest = useCallback(
    (owner: SessionRequestOwner): boolean =>
      mountedRef.current &&
      owner.sessionId.length > 0 &&
      owner.sessionId === viewIdentityRef.current.sessionId &&
      owner.epoch === viewIdentityRef.current.epoch,
    [],
  );

  return {
    adoptSession,
    captureSessionRequest,
    ownsSessionRequest,
  };
};
