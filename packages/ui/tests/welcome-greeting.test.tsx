// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { WelcomeGreeting } from "../src/components/chat/WelcomeGreeting";
afterEach(() => { cleanup(); vi.useRealTimers(); });

test.each([[5, "Good morning."], [11, "Good morning."], [12, "Good afternoon."], [17, "Good afternoon."], [18, "Good evening."], [0, "Good evening."]])("local hour %s selects a short greeting", (hour, greeting) => {
  vi.useFakeTimers(); vi.setSystemTime(new Date(2026, 9, 21, Number(hour)));
  render(<WelcomeGreeting />);
  expect(screen.getByRole("heading", { name: greeting as string })).toBeTruthy();
});

test("a mounted welcome changes at the time boundary and refreshes after sleep", () => {
  vi.useFakeTimers(); vi.setSystemTime(new Date(2026, 9, 21, 11, 59, 59));
  const view = render(<WelcomeGreeting />);
  act(() => vi.advanceTimersByTime(1000));
  expect(screen.getByRole("heading", { name: "Good afternoon." })).toBeTruthy();
  vi.setSystemTime(new Date(2026, 9, 21, 19));
  act(() => window.dispatchEvent(new Event("focus")));
  expect(screen.getByRole("heading", { name: "Good evening." })).toBeTruthy();
  view.unmount();
  expect(vi.getTimerCount()).toBe(0);
});
