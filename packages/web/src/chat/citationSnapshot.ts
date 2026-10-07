export type CitationSummary = { citationId: string; inputRef: string; displayName: string; sourceToolCallId: string; sourceUrl: string };
export type CitationSnapshot = {
  schema: "workspace.citations.v1"; sessionId: string; agentRunId: string;
  throughSequence: number; citations: CitationSummary[];
};

function exact(value: unknown, keys: string[]): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    && Object.keys(value).sort().join(",") === [...keys].sort().join(",");
}

export function validateCitationSnapshot(value: unknown, sessionId: string, agentRunId: string): CitationSnapshot {
  if (!exact(value, ["schema", "sessionId", "agentRunId", "throughSequence", "citations"])
    || value.schema !== "workspace.citations.v1" || value.sessionId !== sessionId || value.agentRunId !== agentRunId
    || !Number.isSafeInteger(value.throughSequence) || (value.throughSequence as number) < 0 || !Array.isArray(value.citations)) {
    throw new Error("citation snapshot invalid");
  }
  const ids = new Set();
  for (const citation of value.citations) {
    if (!exact(citation, ["citationId", "inputRef", "displayName", "sourceToolCallId", "sourceUrl"])
      || [citation.citationId, citation.inputRef, citation.displayName, citation.sourceToolCallId].some((v) => typeof v !== "string" || !v)
      || !/^[A-Za-z0-9:_-]+$/.test(citation.citationId as string)
      || citation.sourceUrl !== `/api/citations/${citation.citationId}` || ids.has(citation.citationId)) {
      throw new Error("citation summary invalid");
    }
    ids.add(citation.citationId);
  }
  return value as CitationSnapshot;
}

export function createCitationRefresher(options: {
  sessionId: string; agentRunId: string; load(signal: AbortSignal): Promise<unknown>;
  apply(snapshot: CitationSnapshot): void;
}) {
  const abort = new AbortController();
  let disposed = false;
  let dirty = false;
  let running: Promise<void> | null = null;
  return {
    request(): Promise<void> {
      if (disposed) return Promise.resolve();
      dirty = true;
      if (running) return running;
      running = (async () => {
        while (dirty && !disposed) {
          dirty = false;
          const value = await options.load(AbortSignal.any([abort.signal, AbortSignal.timeout(10000)]));
          if (!disposed) options.apply(validateCitationSnapshot(value, options.sessionId, options.agentRunId));
        }
      })().finally(() => { running = null; });
      return running;
    },
    dispose() { disposed = true; abort.abort(); },
  };
}
