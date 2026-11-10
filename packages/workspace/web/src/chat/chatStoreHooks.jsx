import { useCallback, useSyncExternalStore } from "react";

export function useAgentRunList(store) {
  return useSyncExternalStore(store.subscribeList, store.getListSnapshot, store.getListSnapshot);
}
export function useAgentRun(store, agentRunId) {
  const subscribe = useCallback((listener) => store.subscribeAgentRun(agentRunId, listener), [store, agentRunId]);
  const getSnapshot = useCallback(() => store.getAgentRunSnapshot(agentRunId), [store, agentRunId]);
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

export function useActivityDisclosures(store, agentRunId) {
  const subscribe = useCallback((listener) => store.subscribeAgentRun(agentRunId, listener), [store, agentRunId]);
  const getSnapshot = useCallback(() => store.getActivityDisclosures(agentRunId), [store, agentRunId]);
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}
