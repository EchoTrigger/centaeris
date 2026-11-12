import { useCallback, useEffect, useRef, useState } from "react";
import { apiResponse } from "../api";
import { parseSessionSubagents, type SessionSubagent } from "./sessionSubagents";

const emptyAgents: readonly SessionSubagent[] = [];

// Read durable children when their parent presentation changes. Token deltas
// neither poll this endpoint nor restart an in-flight read.
export function useSessionSubagents(sessionId: string | null, revision: string, running: boolean) {
  const [snapshot, setSnapshot] = useState<{ sessionId: string; agents: readonly SessionSubagent[] } | null>(null);
  const refreshRef = useRef<() => void>(() => {});
  const refresh = useCallback(() => refreshRef.current(), []);
  useEffect(() => {
    if (!sessionId) return;
    const controller = new AbortController();
    let reading = false;
    let changed = false;
    async function read() {
      if (reading) { changed = true; return; }
      reading = true;
      do {
        changed = false;
        try {
          const response = await apiResponse(`/api/sessions/${encodeURIComponent(sessionId as string)}/subagents`, { signal: controller.signal });
          const agents = parseSessionSubagents(await response.json(), sessionId as string);
          if (!controller.signal.aborted) setSnapshot({ sessionId: sessionId as string, agents });
        } catch (error) {
          if (!controller.signal.aborted) console.error("Session subagents unavailable", { sessionId, error });
        }
      } while (changed && !controller.signal.aborted);
      reading = false;
    }
    refreshRef.current = () => { void read(); };
    const onFocus = () => { if (document.visibilityState !== "hidden") void read(); };
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onFocus);
    return () => {
      controller.abort();
      refreshRef.current = () => {};
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onFocus);
    };
  }, [sessionId]);
  useEffect(() => { void sessionId; void revision; void running; refresh(); }, [sessionId, revision, running, refresh]);
  return { agents: snapshot?.sessionId === sessionId ? snapshot.agents : emptyAgents, refresh };
}
