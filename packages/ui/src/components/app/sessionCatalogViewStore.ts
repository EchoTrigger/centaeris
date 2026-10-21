import { createStore } from "zustand/vanilla";
import type { UiSession } from "../../types/ui";

type SessionCatalogViewState = {
  catalogRevision: number;
  sessions: UiSession[];
  currentSessionId: string | null;
  runningSessionIds: Set<string>;
  completedSessionIds: Set<string>;
  setSessions: (update: UiSession[] | ((previous: UiSession[]) => UiSession[])) => void;
  setCurrentSession: (sessionId: string | null) => void;
  setRunningSessionIds: (update: (previous: Set<string>) => Set<string>) => void;
  setCompletedSessionIds: (update: (previous: Set<string>) => Set<string>) => void;
  advanceCatalogRevision: () => void;
};

// View state belongs to one mounted Desktop owner. Host requests and mutation
// ownership remain in its controller rather than becoming global store effects.
export function createSessionCatalogViewStore() {
  return createStore<SessionCatalogViewState>()(set => ({
    catalogRevision: 0,
    sessions: [],
    currentSessionId: null,
    runningSessionIds: new Set(),
    completedSessionIds: new Set(),
    setSessions: update => set(state => ({ sessions: typeof update === "function" ? update(state.sessions) : update })),
    setCurrentSession: currentSessionId => set({ currentSessionId }),
    setRunningSessionIds: update => set(state => ({ runningSessionIds: update(state.runningSessionIds) })),
    setCompletedSessionIds: update => set(state => ({ completedSessionIds: update(state.completedSessionIds) })),
    advanceCatalogRevision: () => set(state => ({ catalogRevision: state.catalogRevision + 1 })),
  }));
}
