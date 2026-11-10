import { equalSessionRecord, mergeSessionNavigation, type SessionAttention } from './sessionReadState.ts';
import { sortSessionNavigation } from './sessionNavigation.mjs';

type Project = { id: string; name: string };
type Update<T> = T | ((previous: T) => T);
const rowFields = ['id', 'workspaceId', 'projectId', 'currentEnvironmentId', 'executionMode', 'title', 'origin', 'status',
  'deletedAt', 'isPinned', 'isUnread', 'hasActiveAgentRun', 'updatedAt', 'latestAttentionSequence', 'readThroughSequence',
  'attentionRevision', 'isManuallyUnread'];

/** Owned by one mounted user/workspace, independently of the selected Session. */
export function createSessionNavigationStore() {
  let snapshot = { sessions: [] as SessionAttention[], projects: [] as Project[] };
  const listeners = new Set<() => void>();
  const publish = (next: typeof snapshot) => {
    if (next.sessions === snapshot.sessions && next.projects === snapshot.projects) return;
    snapshot = next;
    listeners.forEach(listener => listener());
  };
  return {
    getSnapshot: () => snapshot,
    subscribe(listener: () => void) { listeners.add(listener); return () => { listeners.delete(listener); }; },
    setSessions(update: Update<SessionAttention[]>) {
      const incoming = typeof update === 'function' ? update(snapshot.sessions) : update;
      const rows = incoming.map(record => Object.fromEntries(rowFields.filter(key => Object.hasOwn(record, key))
        .map(key => [key, (record as unknown as Record<string, unknown>)[key]])) as SessionAttention);
      const sessions = sortSessionNavigation(mergeSessionNavigation(snapshot.sessions, rows));
      publish({ ...snapshot, sessions });
    },
    setProjects(update: Update<Project[]>) {
      const incoming = typeof update === 'function' ? update(snapshot.projects) : update;
      const projects = incoming.length === snapshot.projects.length
        && incoming.every((project, index) => equalSessionRecord(project, snapshot.projects[index])) ? snapshot.projects : incoming;
      publish({ ...snapshot, projects });
    },
  };
}
