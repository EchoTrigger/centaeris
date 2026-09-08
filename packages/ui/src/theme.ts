import { useSyncExternalStore } from "react";

export type ThemePreference = "system" | "dark" | "light";
export const THEME_STORAGE_KEY = "centaeris:theme:v1";
function snapshot(): ThemePreference {
  const value = typeof document === "undefined" ? "system" : document.documentElement.dataset.themePreference;
  return value === "dark" || value === "light" ? value : "system";
}
function subscribe(listener: () => void) {
  window.addEventListener("centaeris-theme-change", listener);
  window.addEventListener("storage", listener);
  return () => {
    window.removeEventListener("centaeris-theme-change", listener);
    window.removeEventListener("storage", listener);
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
