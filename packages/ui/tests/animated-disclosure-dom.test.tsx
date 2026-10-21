// @vitest-environment jsdom
import { act } from "react";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { AnimatedDisclosure } from "../src/components/chat/AnimatedDisclosure";

let preference: { matches: boolean; listeners: Set<() => void> };
beforeEach(() => {
  preference = { matches: false, listeners: new Set() };
  vi.stubGlobal("scrollTo", vi.fn());
  vi.stubGlobal("matchMedia", () => ({
    get matches() { return preference.matches; },
    addListener: () => {},
    addEventListener: (_event: string, listener: () => void) => preference.listeners.add(listener),
    removeEventListener: (_event: string, listener: () => void) => preference.listeners.delete(listener),
  }));
  vi.spyOn(HTMLElement.prototype, "scrollHeight", "get").mockReturnValue(100);
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });
const view = (expanded: boolean) => <AnimatedDisclosure expanded={expanded}><button type="button">Read output</button></AnimatedDisclosure>;

test("real Motion opens lazily, preserves reversed output, and releases the final closure", async () => {
  const { container, rerender } = render(view(false));
  expect(screen.queryByRole("button")).toBeNull();
  rerender(view(true));
  const content = screen.getByRole("button");
  const disclosure = container.firstElementChild as HTMLDivElement;
  expect(disclosure.hasAttribute("inert")).toBe(false);
  await waitFor(() => expect(disclosure.style.height).toBe("auto"));
  rerender(view(false));
  expect(disclosure.hasAttribute("inert")).toBe(true);
  expect(content.isConnected).toBe(true);
  rerender(view(true));
  await waitFor(() => expect(disclosure.style.height).toBe("auto"));
  expect(screen.getByRole("button")).toBe(content);
  rerender(view(false));
  await waitFor(() => expect(content.isConnected).toBe(false));
  expect(disclosure.hidden).toBe(true);
});

test("enabling reduced motion during a real closing animation releases content immediately", () => {
  const { container, rerender, unmount } = render(view(true));
  const content = screen.getByRole("button");
  rerender(view(false));
  expect(content.isConnected).toBe(true);
  act(() => {
    preference.matches = true;
    for (const listener of preference.listeners) listener();
  });
  expect(content.isConnected).toBe(false);
  expect((container.firstElementChild as HTMLDivElement).hidden).toBe(true);
  unmount();
  expect(preference.listeners.size).toBe(0);
});
