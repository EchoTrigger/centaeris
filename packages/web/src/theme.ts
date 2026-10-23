import { useStore, createThemeStore, serverThemeStore, THEME_STORAGE_KEY, type ThemePreference } from "@centaeris/frontend-foundations/theme-store";
export { THEME_STORAGE_KEY, type ThemePreference } from "@centaeris/frontend-foundations/theme-store";

const themeStores = new WeakMap<Document, ReturnType<typeof createThemeStore>>();
function currentThemeStore() {
  if (typeof document === "undefined" || typeof window === "undefined") return serverThemeStore;
  let store = themeStores.get(document);
  if (!store) {
    store = createThemeStore(document, window);
    themeStores.set(document, store);
  }
  return store;
}
export function changeTheme(value: ThemePreference) {
  document.documentElement.dataset.themePreference = value;
  try { localStorage.setItem(THEME_STORAGE_KEY, value); } catch { /* In-memory choice still works. */ }
  window.dispatchEvent(new Event("centaeris-theme-change"));
}
export function useThemePreference() {
  return useStore(currentThemeStore(), state => state.preference);
}
export function useResolvedTheme() {
  return useStore(currentThemeStore(), state => state.resolved);
}
let transition: ViewTransition | undefined;
export function changeThemeAnimated(value: ThemePreference, origin: { x: number; y: number }) {
  if (!document.startViewTransition || matchMedia("(prefers-reduced-motion: reduce)").matches || document.hidden) {
    changeTheme(value); return;
  }
  transition?.skipTransition();
  const next = document.startViewTransition(() => changeTheme(value));
  transition = next;
  void next.ready.then(() => {
    const radius = Math.hypot(Math.max(origin.x, innerWidth - origin.x), Math.max(origin.y, innerHeight - origin.y));
    document.documentElement.animate({ clipPath: [`circle(0px at ${origin.x}px ${origin.y}px)`, `circle(${radius}px at ${origin.x}px ${origin.y}px)`] },
      { duration: 520, easing: "cubic-bezier(.22,1,.36,1)", pseudoElement: "::view-transition-new(root)" });
  }).catch(() => {});
  void next.finished.finally(() => { if (transition === next) transition = undefined; }).catch(() => {});
}
