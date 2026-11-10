import { validateCitationSnapshot, type CitationSummary } from "./citationSnapshot.ts";

export type CitationTool = { sourceSequence: string; callId: string };
type Snapshot = { citations: ReadonlyMap<string, readonly CitationSummary[]>; error: boolean };

// A reader belongs to exactly one displayed Session. Each response replaces the
// loaded tool bindings, so replay and reconnect never append duplicate citations.
export function createTranscriptCitationReader(
  sessionId: string,
  load: (sequences: string[], signal: AbortSignal) => Promise<unknown>,
) {
  let snapshot: Snapshot = { citations: new Map(), error: false };
  let disposed = false;
  let revision = 0;
  let abort: AbortController | null = null;
  const listeners = new Set<() => void>();
  return {
    getSnapshot: () => snapshot,
    subscribe(listener: () => void) { listeners.add(listener); return () => { listeners.delete(listener); }; },
    async refresh(tools: readonly CitationTool[]) {
      const request = ++revision;
      abort?.abort();
      const controller = new AbortController();
      abort = controller;
      if (disposed) return;
      try {
        const expected = new Map(tools.map(tool => [tool.sourceSequence, tool.callId]));
        const sequences = [...expected.keys()];
        const citations = new Map<string, readonly CitationSummary[]>();
        for (let offset = 0; offset < sequences.length; offset += 128) {
          const batch = sequences.slice(offset, offset + 128);
          const value = await load(batch, AbortSignal.any([controller.signal, AbortSignal.timeout(10000)])) as {
            sessionId: string; bindings: { sourceSequence: string; sourceToolCallId: string; snapshot: unknown }[];
          };
          if (disposed || request !== revision) return;
          if (!value || value.sessionId !== sessionId || !Array.isArray(value.bindings)
            || Object.keys(value).sort().join(",") !== "bindings,sessionId") throw Error("invalid citation bindings");
          for (const binding of value.bindings) {
            if (!binding || Object.keys(binding).sort().join(",") !== "snapshot,sourceSequence,sourceToolCallId"
              || !batch.includes(binding.sourceSequence) || citations.has(binding.sourceSequence)
              || expected.get(binding.sourceSequence) !== binding.sourceToolCallId) throw Error("invalid citation tool binding");
            const runId = (binding.snapshot as { agentRunId?: unknown })?.agentRunId;
            if (typeof runId !== "string" || !runId) throw Error("missing citation run");
            const validated = validateCitationSnapshot(binding.snapshot, sessionId, runId);
            if (validated.citations.some(c => c.sourceToolCallId !== binding.sourceToolCallId)) throw Error("citation call mismatch");
            citations.set(binding.sourceSequence, validated.citations);
          }
        }
        if (disposed || request !== revision) return;
        snapshot = { citations, error: false };
      } catch {
        if (disposed || request !== revision) return;
        snapshot = { ...snapshot, error: true };
      }
      listeners.forEach(listener => listener());
    },
    dispose() { disposed = true; revision += 1; abort?.abort(); listeners.clear(); },
  };
}
