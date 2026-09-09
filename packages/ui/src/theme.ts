import { useSyncExternalStore } from "react";

export type ThemePreference = "system" | "dark" | "light";
export const THEME_STORAGE_KEY = "centaeris:theme:v1";
function snapshot(): ThemePreference {
  const value = typeof document === "undefined" ? "system" : document.documentElement.dataset.themePreference;
  return value === "dark" || value === "light" ? value : "system";
}
function subscribe(listener: () => void) {
  if (typeof window === "undefined") return () => {};
  window.addEventListener("centaeris-theme-change", listener);
  window.addEventListener("storage", listener);
  window.addEventListener("centaeris-theme-applied", listener);
  return () => {
    window.removeEventListener("centaeris-theme-change", listener);
    window.removeEventListener("storage", listener);
    window.removeEventListener("centaeris-theme-applied", listener);
  };
}
export function changeTheme(value: ThemePreference) {
  document.documentElement.dataset.themePreference = value;
  try { localStorage.setItem(THEME_STORAGE_KEY, value); } catch { /* In-memory choice still works. */ }
  window.dispatchEvent(new Event("centaeris-theme-change"));
}
export function useThemePreference() {
  return useSyncExternalStore(subscribe, snapshot, () => "system" as const);
}

export function useResolvedTheme() {
  return useSyncExternalStore(subscribe, () => typeof document !== "undefined" && document.documentElement.dataset.theme === "dark" ? "dark" : "light", () => "light");
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
