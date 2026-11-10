import { useEffect, useState } from "react";
import { apiResponse } from "../api";
import { createTranscriptCitationReader } from "./transcriptCitations";
import type { TranscriptViewStore } from "./transcriptViewStore";
import type { CitationSummary } from "./citationSnapshot";

const EMPTY: { citations: ReadonlyMap<string, readonly CitationSummary[]>; error: boolean } = { citations: new Map<string, readonly CitationSummary[]>(), error: false };

export function useTranscriptCitations(store: TranscriptViewStore, sessionId: string | null) {
  const [state, setState] = useState({ sessionId, snapshot: EMPTY });
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    void attempt;
    if (!sessionId) return;
    const reader = createTranscriptCitationReader(sessionId, async (sequences, signal) => {
      const query = new URLSearchParams(sequences.map(sequence => ["sequence", sequence]));
      return (await apiResponse(`/api/sessions/${encodeURIComponent(sessionId)}/transcript/citations?${query}`, { signal })).json();
    });
    const unsubscribe = reader.subscribe(() => setState({ sessionId, snapshot: reader.getSnapshot() }));
    let previous = "";
    const refresh = () => {
      const list = store.getListSnapshot();
      if (list.sessionId !== sessionId) return;
      const tools = list.blockIds.flatMap(id => {
        const block = store.getBlockSnapshot(id);
        return block?.body.kind === "tool" && typeof block.body.callId === "string"
          ? [{ sourceSequence: block.orderKey.sourceSequence, callId: block.body.callId }] : [];
      });
      const signature = JSON.stringify([list.viewEpoch, list.appliedSourceHighWater, tools]);
      if (signature === previous) return;
      previous = signature;
      void reader.refresh(tools);
    };
    const unsubscribeList = store.subscribeList(refresh);
    refresh();
    return () => { unsubscribeList(); unsubscribe(); reader.dispose(); };
  }, [sessionId, store, attempt]);
  return { ...(state.sessionId === sessionId ? state.snapshot : EMPTY), retry: () => setAttempt(value => value + 1) };
}
