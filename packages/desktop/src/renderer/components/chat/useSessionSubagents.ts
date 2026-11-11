import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { isNativeHostRuntime, listenHost } from "../../host/hostBridge";
import { listAgentRuns, listSessions, normalizeDesktopAgentEventEnvelope } from "../../lib/chatBridge";
import { useChatViewStore } from "./chatViewStore";
import { mergeSessionSubagents, restoreSessionSubagents } from "./sessionSubagents";
import type { SubagentResult } from "./types";

const lifecycleEvents = new Set(["SubagentSpawned", "SubagentResult", "SubagentFailed", "SubagentCancelled",
  "AgentRunStarted", "AgentRunCompleted", "AgentRunFailed", "AgentRunInterrupted"]);

export function useSessionSubagents(sessionId: string) {
  const live = useChatViewStore(useShallow(state => Object.values(state.subagentById)
    .filter(entry => entry.sessionId === sessionId)));
  const [snapshot, setSnapshot] = useState<{ sessionId: string; entries: SubagentResult[] }>({ sessionId: "", entries: [] });
  const [observed, setObserved] = useState<{ sessionId: string; entries: SubagentResult[] }>({ sessionId: "", entries: [] });
  const [errorSession, setErrorSession] = useState("");
  const refreshRef = useRef<() => void>(() => {});
  const retry = useCallback(() => refreshRef.current(), []);

  useEffect(() => {
    if (!live.length) return;
    setObserved(previous => ({ sessionId, entries: mergeSessionSubagents(previous.sessionId === sessionId ? previous.entries : [], live) }));
  }, [sessionId, live]);

  useEffect(() => {
    if (!sessionId || !isNativeHostRuntime()) return;
    let disposed = false;
    let pending = false;
    let refreshAgain = false;
    let unlisten: (() => void) | undefined;
    let knownSessions = new Set([sessionId]);
    const reportError = (error: unknown) => {
      if (disposed) return;
      console.error("Desktop Agent sessions could not be synchronized", error);
      setErrorSession(sessionId);
    };
    const refresh = async () => {
      if (pending) { refreshAgain = true; return; }
      pending = true;
      try {
        const [sessions, { agentRuns }] = await Promise.all([listSessions(), listAgentRuns({ includeTerminal: true })]);
        if (disposed) return;
        const entries = restoreSessionSubagents(sessionId, sessions, agentRuns);
        knownSessions = new Set([sessionId, ...entries.flatMap(entry => entry.childSessionId ? [entry.childSessionId] : [])]);
        setSnapshot({ sessionId, entries });
        setErrorSession("");
      } catch (error) { reportError(error); }
      finally {
        pending = false;
        if (!disposed && refreshAgain) { refreshAgain = false; void refresh(); }
      }
    };
    refreshRef.current = () => { void refresh(); };
    // Subscribe before reading so a completion racing with hydration is reread.
    void listenHost<unknown>("session/update", value => {
      const update = normalizeDesktopAgentEventEnvelope(value);
      if (!knownSessions.has(update.sessionId)) return;
      const event = update.payload.event;
      if (event && typeof event === "object" && "type" in event && typeof event.type === "string" && lifecycleEvents.has(event.type)) void refresh();
    }).then(release => { if (disposed) release(); else { unlisten = release; void refresh(); } }).catch(reportError);
    return () => { disposed = true; unlisten?.(); refreshRef.current = () => {}; };
  }, [sessionId]);

  const subagents = useMemo(() => mergeSessionSubagents(
    snapshot.sessionId === sessionId ? snapshot.entries : [],
    mergeSessionSubagents(observed.sessionId === sessionId ? observed.entries : [], live),
  ), [snapshot, observed, live, sessionId]);
  return { subagents, error: Boolean(sessionId && errorSession === sessionId), retry };
}
