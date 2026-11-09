import type { TranscriptViewStore } from './transcriptViewStore.ts';

export type SessionView = Readonly<{ cursor: string; restore: ReturnType<TranscriptViewStore['captureView']> }>;

/** A view-local LRU. Its owner is keyed by user and workspace; nothing is persisted. */
export function createSessionViewCache() {
  const entries = new Map<string, SessionView>();
  return {
    get(sessionId: string) {
      const entry = entries.get(sessionId);
      if (entry) { entries.delete(sessionId); entries.set(sessionId, entry); }
      return entry;
    },
    set(sessionId: string, entry: SessionView) {
      entries.delete(sessionId); entries.set(sessionId, entry);
      while (entries.size > 12) entries.delete(entries.keys().next().value!);
    },
    delete(sessionId: string) { entries.delete(sessionId); },
    clear() { entries.clear(); },
  };
}
