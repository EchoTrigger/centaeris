// @vitest-environment jsdom
import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { useRef } from "react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { createWorkspaceNavigation, useNavigationScroll } from "../../src/shell/workspaceNavigationState";

beforeEach(() => {
  localStorage.clear(); sessionStorage.clear();
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.spyOn(HTMLElement.prototype, "getClientRects").mockImplementation(function(this: HTMLElement) {
    return (this.closest("[hidden]") ? [] : [new DOMRect(0, 0, 300, 300)]) as unknown as DOMRectList;
  });
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(300);
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(300);
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

test("folding and desktop visibility survive reload and are isolated by user and workspace", () => {
  const state = createWorkspaceNavigation("user", "workspace");
  state.setCollapsed("projects", true);
  state.setCollapsed("project:one", true);
  state.setDesktopOpen(false);
  const reloaded = createWorkspaceNavigation("user", "workspace");
  expect(reloaded.store.getState()).toEqual({ desktopOpen: false, collapsed: ["projects", "project:one"] });
  expect(createWorkspaceNavigation("other", "workspace").store.getState()).toEqual({ desktopOpen: true, collapsed: [] });
  expect(createWorkspaceNavigation("user", "another").store.getState()).toEqual({ desktopOpen: true, collapsed: [] });
  reloaded.setCollapsed("projects", false);
  expect(reloaded.store.getState().collapsed).toEqual(["project:one"]);
});

test("scroll restoration waits for rows, ignores hidden layout and survives pagehide", () => {
  const state = createWorkspaceNavigation("reader", "workspace");
  state.rememberScroll("conversations", 123);
  function Navigation({ ready, hidden = false }: { ready: boolean; hidden?: boolean }) {
    const ref = useRef<HTMLDivElement>(null);
    useNavigationScroll(ref, state, "conversations", ready);
    return <div hidden={hidden}><div ref={ref} role="region" aria-label="Conversation navigation" /></div>;
  }
  const view = render(<Navigation ready={false} />);
  const element = view.getByRole("region");
  expect(element.scrollTop).toBe(0);
  view.rerender(<Navigation ready />);
  expect(element.scrollTop).toBe(123);
  element.scrollTop = 246;
  fireEvent.scroll(element);
  view.rerender(<Navigation ready hidden />);
  element.scrollTop = 0;
  fireEvent.scroll(element);
  act(() => { window.dispatchEvent(new Event("pagehide")); });
  expect(createWorkspaceNavigation("reader", "workspace").scrollTop("conversations")).toBe(246);
  expect(createWorkspaceNavigation("reader", "workspace").scrollTop("settings")).toBe(0);
});

test("settings and developer navigation keep independent positions", () => {
  const state = createWorkspaceNavigation("settings-reader", null);
  state.rememberScroll("settings", 150);
  state.rememberScroll("developerDocs", 270);
  state.flushScroll();
  const reloaded = createWorkspaceNavigation("settings-reader", null);
  expect(reloaded.scrollTop("settings")).toBe(150);
  expect(reloaded.scrollTop("developerDocs")).toBe(270);
  expect(createWorkspaceNavigation("settings-reader", "workspace").scrollTop("settings")).toBe(0);
});
