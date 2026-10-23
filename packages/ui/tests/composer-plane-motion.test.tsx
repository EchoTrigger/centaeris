// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { cancelFrame, frame } from "motion/react";
import { ComposerPlane } from "../src/components/chat/ComposerPlane";

beforeEach(() => {
  vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} }));
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });
const view = (welcome: boolean, hidden = false, animateDocking = true) => <div hidden={hidden}><ComposerPlane welcome={welcome} animateDocking={animateDocking}><input aria-label="Draft" defaultValue="Keep this input" /></ComposerPlane></div>;
const position = (plane: HTMLElement) => Number.parseFloat(plane.style.top);
function observePositions(plane: HTMLElement) {
  const values: number[] = [];
  const sample = () => { values.push(position(plane)); };
  frame.postRender(sample, true);
  return { values, disconnect: () => cancelFrame(sample) };
}

test("the first response still moves the welcome composer smoothly toward the bottom", async () => {
  const { rerender } = render(view(true));
  const plane = screen.getByRole("region", { name: "Message composer" });
  const input = screen.getByRole("textbox", { name: "Draft" });
  expect(position(plane)).toBe(50);
  rerender(view(false));
  await waitFor(() => { expect(position(plane)).toBeGreaterThan(50); expect(position(plane)).toBeLessThan(100); });
  await waitFor(() => expect(position(plane)).toBe(100));
  expect(screen.getByRole("textbox", { name: "Draft" })).toBe(input);
  expect(input).toHaveProperty("value", "Keep this input");
});

test.each([false, true])("New chat appears centered without return travel; management detour %s", async detour => {
  const { rerender } = render(view(false));
  const plane = screen.getByRole("region", { name: "Message composer" });
  if (detour) rerender(view(false, true));
  const observed = observePositions(plane);
  try {
    rerender(view(true));
    await waitFor(() => expect(observed.values.length).toBeGreaterThan(0));
    expect(observed.values).toEqual(observed.values.map(() => 50));
    expect(position(plane)).toBe(50);
    expect(plane.style.transform).toContain("translateY(-50%)");
  } finally { observed.disconnect(); }
});

test("New chat interrupts a downward transition and stays centered afterward", async () => {
  const { rerender } = render(view(true));
  const plane = screen.getByRole("region", { name: "Message composer" });
  rerender(view(false));
  await waitFor(() => { expect(position(plane)).toBeGreaterThan(50); expect(position(plane)).toBeLessThan(100); });
  const observed = observePositions(plane);
  try {
    rerender(view(true));
    await waitFor(() => expect(observed.values.length).toBeGreaterThan(0));
    expect(observed.values).toEqual(observed.values.map(() => 50));
    await new Promise(resolve => setTimeout(resolve, 80));
    expect(position(plane)).toBe(50);
  } finally { observed.disconnect(); }
});


test.each([false, true])("opening an existing chat docks without welcome travel; management detour %s", async detour => {
  const { rerender } = render(view(true, detour));
  const plane = screen.getByRole("region", { name: "Message composer", hidden: detour });
  const input = screen.getByRole("textbox", { name: "Draft", hidden: detour });
  const observed = observePositions(plane);
  try {
    rerender(view(false, false, false));
    await waitFor(() => expect(observed.values.length).toBeGreaterThan(0));
    expect(observed.values.every(value => value === 100)).toBe(true);
    expect(plane.style.transform).toContain("translateY(-100%)");
    expect(screen.getByRole("textbox", { name: "Draft" })).toBe(input);
  } finally { observed.disconnect(); }
});


test("selecting an existing chat interrupts a first-response docking animation", async () => {
  const { rerender } = render(view(true));
  const plane = screen.getByRole("region", { name: "Message composer" });
  rerender(view(false));
  await waitFor(() => { expect(position(plane)).toBeGreaterThan(50); expect(position(plane)).toBeLessThan(100); });
  const observed = observePositions(plane);
  try {
    rerender(view(false, false, false));
    await waitFor(() => expect(observed.values.length).toBeGreaterThan(0));
    expect(observed.values.every(value => value === 100)).toBe(true);
  } finally { observed.disconnect(); }
});
