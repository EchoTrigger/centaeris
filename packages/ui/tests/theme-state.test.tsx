// @vitest-environment jsdom
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { changeTheme, THEME_STORAGE_KEY, useResolvedTheme, useThemePreference } from "../src/theme";
import { createThemeStore } from "../src/themeStore";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const views: ReactTestRenderer[] = [];
let systemDark = true;
function apply() {
  const preference = document.documentElement.dataset.themePreference;
  document.documentElement.dataset.theme = preference === "system" ? (systemDark ? "dark" : "light") : preference;
  window.dispatchEvent(new Event("centaeris-theme-applied"));
}
function Theme() {
  const preference = useThemePreference();
  const resolved = useResolvedTheme();
  return <span>{preference}:{resolved}</span>;
}
async function mount() {
  let view!: ReactTestRenderer;
  await act(async () => { view = create(<Theme />); views.push(view); });
  return view;
}
beforeEach(() => {
  systemDark = true;
  document.documentElement.dataset.themePreference = "system";
  document.documentElement.dataset.theme = "dark";
  localStorage.clear();
  window.addEventListener("centaeris-theme-change", apply);
});
afterEach(async () => {
  for (const view of views.splice(0)) await act(async () => view.unmount());
  window.removeEventListener("centaeris-theme-change", apply);
  vi.restoreAllMocks();
});
function text(view: ReactTestRenderer) { return view.root.findByType("span").children.join(""); }

test("theme consumers share preferences and follow applied system and cross-window changes", async () => {
  const first = await mount();
  const second = await mount();
  expect(text(first)).toBe("system:dark");
  await act(async () => changeTheme("light"));
  expect(localStorage.getItem(THEME_STORAGE_KEY)).toBe("light");
  expect(text(first)).toBe("light:light");
  expect(text(second)).toBe("light:light");
  await act(async () => {
    document.documentElement.dataset.themePreference = "system";
    systemDark = false;
    apply();
    window.dispatchEvent(new StorageEvent("storage", { key: THEME_STORAGE_KEY, newValue: "system" }));
  });
  expect(text(first)).toBe("system:light");
  await act(async () => { systemDark = true; apply(); });
  expect(text(second)).toBe("system:dark");
});

test("theme remount observes changes made without consumers and unavailable storage keeps the in-memory choice", async () => {
  const first = await mount();
  await act(async () => first.unmount());
  views.splice(views.indexOf(first), 1);
  document.documentElement.dataset.themePreference = "dark";
  document.documentElement.dataset.theme = "dark";
  const remounted = await mount();
  expect(text(remounted)).toBe("dark:dark");
  vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("blocked"); });
  await act(async () => changeTheme("light"));
  expect(text(remounted)).toBe("light:light");
});


test("theme event bridge remains active until the last subscriber leaves and reconnects with the current document", () => {
  const store = createThemeStore(document, window);
  const first = vi.fn();
  const second = vi.fn();
  const unsubscribeFirst = store.subscribe(first);
  const unsubscribeSecond = store.subscribe(second);
  unsubscribeFirst();
  unsubscribeFirst();
  document.documentElement.dataset.themePreference = "light";
  apply();
  expect(first).not.toHaveBeenCalled();
  expect(second).toHaveBeenCalledTimes(1);
  expect(store.getState()).toEqual({ preference: "light", resolved: "light" });
  unsubscribeSecond();
  document.documentElement.dataset.themePreference = "dark";
  apply();
  expect(second).toHaveBeenCalledTimes(1);
  const unsubscribeRemount = store.subscribe(vi.fn());
  expect(store.getState()).toEqual({ preference: "dark", resolved: "dark" });
  unsubscribeRemount();
});
