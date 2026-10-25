import { createStore } from "zustand/vanilla";

export type ThemePreference = "system" | "dark" | "light";
type ThemeState = { preference: ThemePreference; resolved: "dark" | "light" };
export const THEME_STORAGE_KEY = "centaeris:theme:v1";
export const serverThemeStore = createStore<ThemeState>(() => ({ preference: "system", resolved: "light" }));

// theme-init.js owns first paint, OS changes and cross-window storage. This
// store projects that document into React; it does not resolve a second theme.
export function createThemeStore(document: Document, window: Window) {
  const read = (): ThemeState => {
    const preference = document.documentElement.dataset.themePreference;
    return {
      preference: preference === "dark" || preference === "light" ? preference : "system",
      resolved: document.documentElement.dataset.theme === "dark" ? "dark" : "light",
    };
  };
  const store = createStore<ThemeState>(read);
  const refresh = () => {
    const next = read();
    const previous = store.getState();
    if (previous.preference !== next.preference || previous.resolved !== next.resolved) store.setState(next);
  };
  const onStorage = (event: StorageEvent) => {
    if (event.key === THEME_STORAGE_KEY || event.key === null) refresh();
  };
  const subscribe = store.subscribe;
  let subscribers = 0;
  // One platform-event bridge per document, retained only while React has
  // consumers. Zustand owns snapshots, selector updates and subscriber delivery.
  store.subscribe = (listener) => {
    if (subscribers++ === 0) {
      window.addEventListener("centaeris-theme-change", refresh);
      window.addEventListener("centaeris-theme-applied", refresh);
      window.addEventListener("storage", onStorage);
      refresh();
    }
    const unsubscribe = subscribe(listener);
    let subscribed = true;
    return () => {
      if (!subscribed) return;
      subscribed = false;
      unsubscribe();
      if (--subscribers === 0) {
        window.removeEventListener("centaeris-theme-change", refresh);
        window.removeEventListener("centaeris-theme-applied", refresh);
        window.removeEventListener("storage", onStorage);
      }
    };
  };
  return store;
}

export { useStore } from "zustand";
