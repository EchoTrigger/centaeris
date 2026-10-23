// @vitest-environment jsdom
import { act } from "react";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { DesktopShell } from "../src/components/DesktopShell";

let reduced = false;
let listeners: Set<() => void>;
beforeEach(() => {
  reduced = false; listeners = new Set();
  vi.stubGlobal("matchMedia", (query: string) => ({ get matches() { return reduced; },
    // Track this component bridge separately from Motion's shared device listener.
    addEventListener: (_event: string, listener: () => void) => { if (query.includes(": reduce")) listeners.add(listener); },
    removeEventListener: (_event: string, listener: () => void) => { if (query.includes(": reduce")) listeners.delete(listener); },
  }));
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });
const view = (expanded: boolean) => <DesktopShell expanded={expanded} className="test-shell"><button type="button">Workspace content</button></DesktopShell>;
function progress(element: HTMLElement) { return Number(element.style.getPropertyValue("--sidebar-progress")); }

test("real Motion changes sidebar width without remounting workspace content and can reverse in place", async () => {
  const { container, rerender } = render(view(true));
  const shell = container.firstElementChild as HTMLElement;
  const content = screen.getByRole("button");
  expect(progress(shell)).toBe(1);
  rerender(view(false));
  await waitFor(() => { expect(progress(shell)).toBeGreaterThan(0); expect(progress(shell)).toBeLessThan(1); });
  rerender(view(true));
  await waitFor(() => expect(progress(shell)).toBe(1));
  expect(screen.getByRole("button")).toBe(content);
  rerender(view(false));
  await waitFor(() => expect(progress(shell)).toBe(0));
  await waitFor(() => expect(listeners.size).toBe(0));
});

test("live reduced motion finishes sidebar resizing immediately and future toggles stay immediate", async () => {
  const { container, rerender } = render(view(true));
  const shell = container.firstElementChild as HTMLElement;
  rerender(view(false));
  await waitFor(() => expect(progress(shell)).toBeLessThan(1));
  act(() => { reduced = true; for (const listener of listeners) listener(); });
  expect(progress(shell)).toBe(0);
  rerender(view(true));
  expect(progress(shell)).toBe(1);
});

test("unmount removes the preference listener and a new shell keeps independent animation state", async () => {
  const { container, rerender, unmount } = render(view(true));
  const shell = container.firstElementChild as HTMLElement;
  rerender(view(false));
  await waitFor(() => expect(progress(shell)).toBeLessThan(1));
  unmount();
  expect(listeners.size).toBe(0);
  const remounted = render(view(true));
  const current = remounted.container.firstElementChild as HTMLElement;
  await new Promise(resolve => setTimeout(resolve, 60));
  expect(progress(current)).toBe(1);
});
