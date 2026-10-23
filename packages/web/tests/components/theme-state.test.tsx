// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { changeTheme, THEME_STORAGE_KEY, useResolvedTheme, useThemePreference } from "../../src/theme";

function Theme() { return <output>{useThemePreference()}:{useResolvedTheme()}</output>; }
const apply = () => {
  const preference = document.documentElement.dataset.themePreference;
  document.documentElement.dataset.theme = preference === "system" ? "dark" : preference;
  window.dispatchEvent(new Event("centaeris-theme-applied"));
};
beforeEach(() => {
  localStorage.clear();
  document.documentElement.dataset.themePreference = "system";
  apply();
  window.addEventListener("centaeris-theme-change", apply);
});
afterEach(() => { cleanup(); window.removeEventListener("centaeris-theme-change", apply); vi.restoreAllMocks(); });

test("Web theme consumers retain system, selected and cross-window preferences", () => {
  render(<><Theme /><Theme /></>);
  expect(screen.getAllByText("system:dark")).toHaveLength(2);
  act(() => changeTheme("light"));
  expect(localStorage.getItem(THEME_STORAGE_KEY)).toBe("light");
  expect(screen.getAllByText("light:light")).toHaveLength(2);
  act(() => {
    document.documentElement.dataset.themePreference = "dark";
    document.documentElement.dataset.theme = "dark";
    window.dispatchEvent(new StorageEvent("storage", { key: THEME_STORAGE_KEY }));
  });
  expect(screen.getAllByText("dark:dark")).toHaveLength(2);
});

test("Web theme keeps its in-memory selection when storage is unavailable", () => {
  render(<Theme />);
  vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new DOMException("Unavailable", "QuotaExceededError"); });
  act(() => changeTheme("light"));
  expect(screen.getByText("light:light")).toBeTruthy();
});
