import { createStore, useStore } from "@centaeris/ui/state";
import { useLayoutEffect, useRef, useSyncExternalStore, type RefObject } from "react";

type Preferences = Readonly<{ desktopOpen: boolean; collapsed: readonly string[] }>;
type ScrollView = "conversations" | "settings" | "developerDocs";
type ScrollPositions = Partial<Record<ScrollView, number>>;
const defaults: Preferences = { desktopOpen: true, collapsed: [] };

export function createWorkspaceNavigation(userId: string, workspaceId: string | null) {
  const scope = JSON.stringify([userId, workspaceId]);
  const preferenceKey = `centaeris:workspace-navigation:v1:${scope}`;
  const scrollKey = `centaeris:workspace-navigation-scroll:v1:${scope}`;
  let preferences = defaults;
  let positions: ScrollPositions = {};
  try {
    const saved = JSON.parse(localStorage.getItem(preferenceKey) || "null");
    if (saved && Object.keys(saved).sort().join("|") === "collapsed|desktopOpen"
      && typeof saved.desktopOpen === "boolean" && Array.isArray(saved.collapsed)
      && saved.collapsed.every((key: unknown) => typeof key === "string")) preferences = saved;
    const scroll = JSON.parse(sessionStorage.getItem(scrollKey) || "null");
    if (scroll && typeof scroll === "object" && !Array.isArray(scroll)
      && Object.entries(scroll).every(([key, value]) => ["conversations", "settings", "developerDocs"].includes(key)
      && typeof value === "number" && Number.isFinite(value) && value >= 0)) positions = scroll;
  } catch { /* Browser preferences are optional, as with the existing input preferences. */ }
  const store = createStore<Preferences>(() => preferences);
  function update(next: Preferences) {
    store.setState(next);
    try { localStorage.setItem(preferenceKey, JSON.stringify(next)); }
    catch { /* Keep the current document usable when storage is unavailable. */ }
  }
  return {
    store,
    setDesktopOpen(desktopOpen: boolean) { update({ ...store.getState(), desktopOpen }); },
    setCollapsed(key: string, collapsed: boolean) {
      const current = store.getState();
      if (current.collapsed.includes(key) === collapsed) return;
      update({ ...current, collapsed: collapsed ? [...current.collapsed, key] : current.collapsed.filter(value => value !== key) });
    },
    scrollTop(view: ScrollView) { return positions[view] ?? 0; },
    rememberScroll(view: ScrollView, top: number) { positions[view] = top; },
    flushScroll() {
      try { sessionStorage.setItem(scrollKey, JSON.stringify(positions)); }
      catch { /* The in-document position still survives route changes. */ }
    },
  };
}

type WorkspaceNavigation = ReturnType<typeof createWorkspaceNavigation>;
const scopes = new Map<string, WorkspaceNavigation>();

// Only preference flags, opaque project identities and offsets are retained.
// Session selection and authorization continue to belong to the current route.
export function useWorkspaceNavigation(userId: string, workspaceId: string | null) {
  const scope = JSON.stringify([userId, workspaceId]);
  let navigation = scopes.get(scope);
  if (!navigation) { navigation = createWorkspaceNavigation(userId, workspaceId); scopes.set(scope, navigation); }
  return navigation;
}

export function useSidebarOpen(navigation: WorkspaceNavigation) {
  return useStore(navigation.store, state => state.desktopOpen);
}

export function useCollapsedNavigation(navigation: WorkspaceNavigation) {
  return useStore(navigation.store, state => state.collapsed);
}

export function useNavigationScroll(
  ref: RefObject<HTMLElement | null>, navigation: WorkspaceNavigation, view: ScrollView, ready = true,
) {
  const latest = useRef(navigation.scrollTop(view));
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element || !ready) return;
    latest.current = navigation.scrollTop(view);
    let visible = false;
    let restoring = false;
    const isVisible = () => element.getClientRects().length > 0 && element.clientHeight > 0 && element.clientWidth > 0;
    const restore = () => {
      const nextVisible = isVisible();
      if (nextVisible && !visible) {
        restoring = true;
        element.scrollTop = latest.current;
        restoring = false;
      }
      visible = nextVisible;
    };
    const remember = () => {
      if (restoring || !isVisible()) return;
      latest.current = element.scrollTop;
      navigation.rememberScroll(view, latest.current);
    };
    const flush = () => navigation.flushScroll();
    const visibility = () => { if (document.hidden) flush(); };
    restore();
    const observer = new ResizeObserver(restore);
    observer.observe(element);
    element.addEventListener("scroll", remember, { passive: true });
    window.addEventListener("pagehide", flush);
    document.addEventListener("visibilitychange", visibility);
    return () => {
      observer.disconnect();
      element.removeEventListener("scroll", remember);
      window.removeEventListener("pagehide", flush);
      document.removeEventListener("visibilitychange", visibility);
      flush();
    };
  }, [ref, navigation, view, ready]);
}

function subscribeViewport(listener: () => void) {
  window.addEventListener("resize", listener);
  return () => window.removeEventListener("resize", listener);
}

export function useCompactWorkspaceLayout() {
  return useSyncExternalStore(subscribeViewport, () => window.innerWidth <= 760);
}
