import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError } from "../api";
import { WorkspacePresentationController } from "../chat/workspacePresentationController";
import { streamWorkspacePresentation } from "../chat/workspacePresentationTransport";
import { createTranscriptViewStore } from "../chat/transcriptViewStore";
import type { PresentationSession } from "../chat/workspacePresentationProtocol";

const empty = { scope: "", historyRevision: "0", workRevision: "0", navigation: null as readonly PresentationSession[] | null, error: false };
/** The Agent screen shares one agent-scoped subscription for navigation and
 * invalidation. Agent history keeps its own authoritative projection and cursor.
 */
export function useAgentPresentation(workspaceId: string, agentId: string, onAuthorityLost: () => void) {
  const scope = JSON.stringify([workspaceId, agentId]);
  const [view, setView] = useState(empty);
  const [attempt, setAttempt] = useState(0);
  const attemptRef = useRef(attempt); attemptRef.current = attempt;
  const lostRef = useRef(onAuthorityLost); lostRef.current = onAuthorityLost;
  useEffect(() => {
    const abort = new AbortController();
    const isCurrent = () => !abort.signal.aborted && attemptRef.current === attempt;
    const controller = new WorkspacePresentationController({
      workspaceId, agentId, sessionId: null, signal: abort.signal, store: createTranscriptViewStore(),
      onNavigation: navigation => { if (isCurrent()) setView(previous => ({ ...(previous.scope === scope ? previous : empty), scope, navigation })); },
      onViewRevision: revision => { if (isCurrent()) setView(previous => ({ ...(previous.scope === scope ? previous : empty), scope,
        historyRevision: revision.historyRevision, workRevision: revision.workRevision })); },
    });
    void streamWorkspacePresentation({
      workspaceId, agentId, sessionId: null, controller, signal: abort.signal,
      onConnection: connection => {
        if (isCurrent() && connection === "running") setView(previous => ({ ...(previous.scope === scope ? previous : empty), scope, error: false }));
      },
    }).catch(error => {
      if (!isCurrent()) return;
      if (error instanceof ApiError && [401, 403, 404].includes(error.status)) {
        setView({ ...empty, scope, navigation: [], error: true }); lostRef.current();
      } else setView(previous => ({ ...(previous.scope === scope ? previous : empty), scope, error: true }));
    });
    return () => { abort.abort(); controller.dispose(); };
  }, [workspaceId, agentId, scope, attempt]);
  const retry = useCallback(() => setAttempt(value => value + 1), []);
  return { ...(view.scope === scope ? view : empty), retry };
}
