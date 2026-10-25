import { animate } from "@centaeris/frontend-foundations/motion-mini";
import { useStore } from "zustand";
import { createThemeStore, serverThemeStore, THEME_STORAGE_KEY, type ThemePreference } from "./themeStore";
export { THEME_STORAGE_KEY, type ThemePreference } from "./themeStore";

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
const activeThemeTransitions = new WeakMap<Document, () => void>();
export function changeThemeAnimated(value: ThemePreference, origin: { x: number; y: number }) {
  const ownerDocument = document;
  activeThemeTransitions.get(ownerDocument)?.();
  const preference = window.matchMedia("(prefers-reduced-motion: reduce)");
  if (!ownerDocument.startViewTransition || preference.matches || ownerDocument.hidden) {
    changeTheme(value);
    return;
  }

  let current = true;
  let transition: ViewTransition | undefined;
  let playback: ReturnType<typeof animate> | undefined;
  const cleanup = () => {
    current = false;
    preference.removeEventListener("change", onPreferenceChange);
    ownerDocument.removeEventListener("visibilitychange", onVisibilityChange);
    if (activeThemeTransitions.get(ownerDocument) === cancel) activeThemeTransitions.delete(ownerDocument);
  };
  const cancel = () => {
    cleanup();
    // Pseudo-element styles cannot be committed to the live document. Motion's
    // cancel removes the snapshot animation; stop intentionally preserves it.
    playback?.cancel();
    transition?.skipTransition();
  };
  const fallback = () => {
    if (!current) return;
    cancel();
    changeTheme(value);
  };
  const onPreferenceChange = () => { if (preference.matches) fallback(); };
  const onVisibilityChange = () => { if (ownerDocument.hidden) fallback(); };
  activeThemeTransitions.set(ownerDocument, cancel);
  preference.addEventListener("change", onPreferenceChange);
  ownerDocument.addEventListener("visibilitychange", onVisibilityChange);
  try {
    transition = ownerDocument.startViewTransition(() => { if (current) changeTheme(value); });
  } catch {
    fallback();
    return;
  }
  void transition.ready.then(() => {
    if (!current) return;
    if (preference.matches || ownerDocument.hidden) { fallback(); return; }
    const radius = Math.hypot(Math.max(origin.x, innerWidth - origin.x), Math.max(origin.y, innerHeight - origin.y));
    // Motion 14.4.0's mini runtime forwards pseudoElement to native animation,
    // but its AnimationOptions declaration omits this supported field.
    const options: NonNullable<Parameters<typeof animate>[2]> & { pseudoElement: string } = {
      duration: 0.52, ease: [0.22, 1, 0.36, 1], pseudoElement: "::view-transition-new(root)",
    };
    try {
      playback = animate(ownerDocument.documentElement,
        { clipPath: [`circle(0px at ${origin.x}px ${origin.y}px)`, `circle(${radius}px at ${origin.x}px ${origin.y}px)`] }, options);
    } catch { fallback(); }
  }, fallback);
  void transition.finished.then(() => {
    if (!current) return;
    cleanup();
    playback?.cancel();
  }, fallback);
}
