import { useEffect, useState } from "react";
import { apiResponse } from "../api";

export type PublishedArtifact = { artifactRef: string; filename: string; downloadUrl: string };
type TurnMetadata = { startedAtMs: number; completedAtMs: number | null; artifacts: PublishedArtifact[] };

// Only turn boundaries currently loaded by the paged transcript are requested.
// Token deltas and tool progress do not refresh or restart these clocks.
export function useTranscriptTurnMetadata(sessionId: string | null, anchors: string, running: boolean) {
  const [snapshot, setSnapshot] = useState<{ sessionId: string | null; times: Map<string, TurnMetadata> }>({ sessionId: null, times: new Map() });
  useEffect(() => {
    void running; // Completion refreshes final timestamps and published files.
    if (!sessionId || !anchors) return;
    const controller = new AbortController();
    const sequences = anchors.split(",");
    void (async () => {
      const times = new Map<string, TurnMetadata>();
      for (let offset = 0; offset < sequences.length; offset += 128) {
        const query = new URLSearchParams(sequences.slice(offset, offset + 128).map((sequence) => ["sequence", sequence]));
        const data = await (await apiResponse(`/api/sessions/${encodeURIComponent(sessionId)}/transcript/turn-metadata?${query}`, { signal: controller.signal })).json();
        if (!data || !Array.isArray(data.turns)) throw new Error("invalid transcript work times");
        for (const time of data.turns) {
          if (!sequences.includes(time.anchorSequence) || !Number.isSafeInteger(time.startedAtMs)
            || (time.completedAtMs !== null && !Number.isSafeInteger(time.completedAtMs))) throw new Error("invalid transcript work time");
          if (!Array.isArray(time.artifacts) || time.artifacts.some((artifact: PublishedArtifact) =>
            typeof artifact.artifactRef !== "string" || typeof artifact.filename !== "string"
            || artifact.downloadUrl !== `/api/artifacts/${artifact.artifactRef.replace(/^artifact:/, "")}/download`)) {
            throw new Error("invalid published artifact metadata");
          }
          times.set(time.anchorSequence, time);
        }
      }
      if (!controller.signal.aborted) setSnapshot({ sessionId, times });
    })().catch((error) => {
      if (!controller.signal.aborted) console.error("Transcript work times unavailable", { sessionId, error });
    });
    return () => controller.abort();
  }, [sessionId, anchors, running]);
  return snapshot.sessionId === sessionId ? snapshot.times : new Map<string, TurnMetadata>();
}
