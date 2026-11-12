import { useEffect, useMemo, useRef, useState } from "react";
import { createTranscriptViewStore } from "./transcriptViewStore";
import type { createWorkspaceTranscriptTransport } from "./transcriptTransport";

// A historical search window is independent of the live tail. Their separated
// blocks must never appear adjacent as if the intervening history were loaded.
export function useTranscriptSearchTarget(sessionId: string, eventId: string, transport: ReturnType<typeof createWorkspaceTranscriptTransport>) {
  const [attempt, setAttempt] = useState(0);
  const { store } = useMemo(() => ({ sessionId, eventId, attempt, store: createTranscriptViewStore() }), [sessionId, eventId, attempt]);
  const [result, setResult] = useState({ store, blockId: "", loading: Boolean(eventId), error: false });
  const [older, setOlder] = useState({ store, loading: false, error: false });
  const olderRequest = useRef<AbortController | null>(null);
  useEffect(() => {
    if (!eventId || !sessionId) return;
    const controller = new AbortController();
    setResult({ store, blockId: "", loading: true, error: false });
    void transport.loadTarget(sessionId, eventId, controller.signal).then(target => {
      if (controller.signal.aborted) return;
      store.openTail(target.page);
      setResult({ store, blockId: target.targetBlockId, loading: false, error: false });
    }).catch(() => {
      if (!controller.signal.aborted) setResult({ store, blockId: "", loading: false, error: true });
    });
    return () => { controller.abort(); olderRequest.current?.abort(); olderRequest.current = null; };
  }, [sessionId, eventId, transport, store]);
  async function loadOlder() {
    const current = store.getListSnapshot();
    if (olderRequest.current || !current.olderCursor || !current.projectionGeneration || !current.projectionVersion) return;
    const controller = new AbortController();
    olderRequest.current = controller;
    setOlder({ store, loading: true, error: false });
    try {
      const page = await transport.loadOlder({ sessionId, projectionVersion: current.projectionVersion,
        projectionGeneration: current.projectionGeneration, sourceHighWater: current.sourceHighWater }, current.olderCursor, controller.signal);
      if (!controller.signal.aborted) { store.prependPage(page, current.viewEpoch); setOlder({ store, loading: false, error: false }); }
    } catch {
      if (!controller.signal.aborted) setOlder({ store, loading: false, error: true });
    } finally { if (olderRequest.current === controller) olderRequest.current = null; }
  }
  return { store, blockId: result.store === store ? result.blockId : "",
    loading: result.store !== store || result.loading, error: result.store === store && result.error,
    loadingOlder: older.store === store && older.loading, olderError: older.store === store && older.error,
    loadOlder, retry: () => setAttempt(value => value + 1) };
}
