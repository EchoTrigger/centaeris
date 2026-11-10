import { listenHost } from "../../host/hostBridge";

type Options = {
  sessionId: string;
  isKnown: (runId: string) => boolean;
  canObserve: () => boolean;
  recover: (isCurrent: () => boolean) => Promise<void>;
  onError: (message: string) => void;
};

// A notification only triggers authoritative hydration; it is not itself a
// transcript or an authority to replace a different workspace's view.
export function observeBackgroundRuns(options: Options): () => void {
  let disposed = false;
  let busy = false;
  let unsubscribe: (() => void) | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const pending = new Map<string, number>();
  let revision = 0;
  let failures = 0;
  const pump = async () => {
    timer = undefined;
    if (disposed || busy) return;
    for (const id of pending.keys()) if (options.isKnown(id)) pending.delete(id);
    if (!pending.size) return;
    if (!options.canObserve()) { timer = setTimeout(pump, 250); return; }
    busy = true;
    const observed = new Map(pending);
    const consumeObserved = () => {
      for (const [id, version] of observed) if (pending.get(id) === version) pending.delete(id);
    };
    try {
      await options.recover(() => !disposed && options.canObserve());
      if (!disposed) { consumeObserved(); failures = 0; }
    } catch (error) {
      if (!disposed && options.canObserve()) {
        failures += 1;
        if (failures >= 2 || !/timed? ?out|disconnected|ECONNRESET|EPIPE|temporarily unavailable/i.test(String(error))) {
          consumeObserved();
          options.onError(String(error));
          failures = 0;
        }
      }
    } finally {
      busy = false;
      if (!disposed && pending.size) timer = setTimeout(pump, 500);
    }
  };
  void listenHost<{ sessionId?: string; agentRunId?: string }>("session/update", (event) => {
    if (disposed || event.sessionId !== options.sessionId || !event.agentRunId || options.isKnown(event.agentRunId)) return;
    pending.set(event.agentRunId, ++revision);
    if (!busy && timer === undefined) timer = setTimeout(pump, 0);
  }).then((stop) => { if (disposed) stop(); else unsubscribe = stop; }).catch((error) => {
    if (!disposed) options.onError(String(error));
  });
  return () => { disposed = true; clearTimeout(timer); unsubscribe?.(); };
}
