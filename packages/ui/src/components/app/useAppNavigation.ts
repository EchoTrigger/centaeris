import { useCallback, useRef, useState } from "react";
export type AppPage = "chat" | "schedules" | "plugins" | "settings" | "about";
export type AppLocation = { page: AppPage; pluginId?: string; resourceTab?: "plugins" | "skills"; sessionId: string | null; root: string | null };
export function useAppNavigation(current: AppLocation, apply: (location: AppLocation, isCurrent: () => boolean) => Promise<boolean>) {
  const entries = useRef<AppLocation[]>([current]);
  const cursor = useRef(0);
  const busy = useRef(false);
  const epoch = useRef(0);
  const inputs = useRef({ current, apply });
  inputs.current = { current, apply };
  const [revision, setRevision] = useState(0);
  const move = useCallback(async (target: AppLocation | number, origin?: AppLocation) => {
    const owner = ++epoch.current;
    const index = typeof target === "number" ? cursor.current + target : null;
    const next = index === null ? target as AppLocation : entries.current[index];
    if (!next) return;
    entries.current[cursor.current] = origin ?? inputs.current.current;
    busy.current = true;
    setRevision(v => v + 1);
    try {
      if (!await inputs.current.apply(next, () => owner === epoch.current) || owner !== epoch.current) return;
      if (index !== null) cursor.current = index;
      else {
        entries.current = entries.current.slice(0, cursor.current + 1);
        entries.current.push(next);
        cursor.current++;
      }
    } finally { if (owner === epoch.current) { busy.current = false; setRevision(v => v + 1); } }
  }, []);
  const accept = useCallback((next: AppLocation, origin: AppLocation) => {
    epoch.current++; busy.current = false;
    entries.current[cursor.current] = origin;
    entries.current = entries.current.slice(0, cursor.current + 1);
    entries.current.push(next); cursor.current++;
    setRevision(v => v + 1);
  }, []);
  return { accept, move, busy: busy.current, canBack: cursor.current > 0 && !busy.current, canForward: cursor.current < entries.current.length - 1 && !busy.current, revision };
}
