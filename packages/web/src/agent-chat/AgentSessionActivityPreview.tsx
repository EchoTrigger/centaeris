import { useCallback, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "../i18n";
import { TranscriptBlockList } from "../chat/TranscriptBlockList";
import { createTranscriptViewStore } from "../chat/transcriptViewStore";
import { createWorkspaceTranscriptTransport } from "../chat/transcriptTransport";

// This owns a separate readonly snapshot. The ordinary Session renderer,
// transport, stores and live stream implementation remain unchanged.
export function AgentSessionActivityPreview({ sessionId, onRunState }: Readonly<{
  sessionId: string;
  onRunState: (sessionId: string, state: "running" | "unknown") => void;
}>) {
  const { t } = useTranslation();
  const [store] = useState(createTranscriptViewStore);
  const [transport] = useState(createWorkspaceTranscriptTransport);
  const [loading, setLoading] = useState(true);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [error, setError] = useState(false);
  const [authorized, setAuthorized] = useState(false);
  const signalRef = useRef<AbortSignal | null>(null);
  const controllerRef = useRef<AbortController | null>(null);
  const loadedPagesRef = useRef(1);
  const loadedSessionRef = useRef<string | null>(null);
  const load = useCallback(async () => {
    controllerRef.current?.abort();
    const controller = new AbortController();
    controllerRef.current = controller;
    signalRef.current = controller.signal;
    setLoading(true); setError(false); setLoadingOlder(false);
    try {
      const page = await transport.loadTail(sessionId, controller.signal);
      if (controller.signal.aborted) return;
      const identity = {
        sessionId, projectionVersion: page.projectionVersion,
        projectionGeneration: page.projectionGeneration, sourceHighWater: page.sourceHighWater,
      };
      const olderPages = [];
      let cursor = page.olderCursor;
      while (olderPages.length + 1 < loadedPagesRef.current && cursor && !controller.signal.aborted) {
        const olderPage = await transport.loadOlder(identity, cursor, controller.signal);
        olderPages.push(olderPage); cursor = olderPage.olderCursor;
      }
      const active = await transport.loadActiveAgentRun(identity, controller.signal);
      if (!controller.signal.aborted) {
        const epoch = store.openTail(page);
        for (const olderPage of olderPages) store.prependPage(olderPage, epoch);
        loadedPagesRef.current = olderPages.length + 1;
        setAuthorized(true); onRunState(sessionId, active.agentRun?.status === "running" ? "running" : "unknown");
      }
    } catch { if (!controller.signal.aborted) { store.clear(); setAuthorized(false); setError(true); onRunState(sessionId, "unknown"); } }
    finally { if (!controller.signal.aborted) setLoading(false); }
  }, [sessionId, store, transport, onRunState]);
  useLayoutEffect(() => {
    if (loadedSessionRef.current !== sessionId) { loadedSessionRef.current = sessionId; loadedPagesRef.current = 1; store.clear(); }
    setAuthorized(false);
    void load();
    return () => { controllerRef.current?.abort(); setAuthorized(false); };
  }, [load, sessionId, store]);

  const loadOlder = useCallback(async () => {
    const signal = signalRef.current;
    const snapshot = store.getListSnapshot();
    if (!authorized || loading || !signal || signal.aborted || loadingOlder || !snapshot.olderCursor || !snapshot.projectionVersion || !snapshot.projectionGeneration) return;
    setLoadingOlder(true);
    try {
      const page = await transport.loadOlder({
        sessionId, projectionVersion: snapshot.projectionVersion,
        projectionGeneration: snapshot.projectionGeneration, sourceHighWater: snapshot.sourceHighWater,
      }, snapshot.olderCursor, signal);
      if (!signal.aborted && store.prependPage(page, snapshot.viewEpoch)) loadedPagesRef.current++;
    } catch { if (!signal.aborted) setError(true); }
    finally { if (!signal.aborted) setLoadingOlder(false); }
  }, [sessionId, loadingOlder, loading, authorized, store, transport]);
  return <div className="agentChatRouteActivity">
    {error ? <div role="alert"><p>{t("agentChat.previewError")}</p><button type="button" onClick={() => void load()}>{t("agentChat.retry")}</button></div> : <div className="agentPreviewFiles" aria-hidden={!authorized} inert={!authorized}><TranscriptBlockList
      store={store} sessionId={sessionId} loadingHistory={loading} loadingOlderHistory={loadingOlder}
      onLoadOlderHistory={loadOlder} emptyState={<p>{t("agentChat.noActivity")}</p>}
    /></div>}
  </div>;
}
