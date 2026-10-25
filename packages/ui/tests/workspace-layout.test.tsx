// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { useState } from "react";
import { readFileSync } from "node:fs";
import { URL as FileURL } from "node:url";
const layoutCss = readFileSync(new FileURL("../src/styles/desktop-layout.css", import.meta.url), "utf8");
import { WorkspaceLayout } from "../src/components/WorkspaceLayout";

let width = 1200;
const observers = new Set<{ callback: ResizeObserverCallback; targets: Set<Element> }>();
beforeEach(() => {
  width = 1200;
  vi.stubGlobal("PointerEvent", class extends MouseEvent {
    pointerId: number;
    pointerType: string;
    isPrimary: boolean;
    constructor(type: string, init: PointerEventInit = {}) { super(type, init); this.pointerId = init.pointerId ?? 1; this.pointerType = init.pointerType ?? "mouse"; this.isPrimary = init.isPrimary ?? true; }
  });
  vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
  vi.stubGlobal("ResizeObserver", class {
    targets = new Set<Element>();
    constructor(public callback: ResizeObserverCallback) { observers.add(this); }
    observe(target: Element) { this.targets.add(target); }
    unobserve(target: Element) { this.targets.delete(target); }
    disconnect() { this.targets.clear(); observers.delete(this); }
  });
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockImplementation(function (this: HTMLElement) {
    if (this.hasAttribute("data-panel")) return width * Number(this.style.flexGrow || 0) / 100;
    if (this.hasAttribute("data-separator")) return 0;
    return width;
  });
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(800);
  vi.spyOn(HTMLElement.prototype, "offsetLeft", "get").mockImplementation(function (this: HTMLElement) {
    return this.hasAttribute("data-separator") || this.id === "preview" ? Number(this.parentElement?.querySelector<HTMLElement>("[data-panel]")?.offsetWidth ?? 0) : 0;
  });
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    const left = this.hasAttribute("data-separator") ? Number(this.parentElement?.querySelector<HTMLElement>("[data-panel]")?.offsetWidth ?? 0)
      : this.id === "preview" ? width - this.offsetWidth : 0;
    return new DOMRect(left, 0, this.offsetWidth, 800);
  });
});
afterEach(() => { cleanup(); observers.clear(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });
async function resize(nextWidth: number) {
  width = nextWidth;
  await act(async () => {
    for (const observer of [...observers]) observer.callback([...observer.targets].map(target => ({ target, contentRect: target.getBoundingClientRect(), devicePixelContentBoxSize: [{ inlineSize: (target as HTMLElement).offsetWidth, blockSize: 800 }], borderBoxSize: [{ inlineSize: (target as HTMLElement).offsetWidth, blockSize: 800 }], contentBoxSize: [{ inlineSize: (target as HTMLElement).offsetWidth, blockSize: 800 }] }) as ResizeObserverEntry), {} as ResizeObserver);
  });
}
function Counter({ name }: { name: string }) {
  const [count, setCount] = useState(0);
  return <button onClick={() => setCount(count + 1)}>{name} {count}</button>;
}
const content = (visible = true) => <WorkspaceLayout previewVisible={visible} preview={<aside aria-label="Preview"><Counter name="Preview" /></aside>}><main aria-label="Chat"><Counter name="Chat" /></main></WorkspaceLayout>;
const separator = () => screen.getByRole("separator", { name: "Resize workspace panel" });
const split = () => Number(separator().getAttribute("aria-valuenow"));

test("library keyboard resizing preserves both minimum widths and supports equal split reset", async () => {
  render(content());
  await resize(width);
  const initial = split();
  expect(initial).toBe(50);
  fireEvent.keyDown(separator(), { key: "ArrowLeft" });
  await waitFor(() => expect(split()).toBeLessThan(initial));
  fireEvent.doubleClick(separator());
  await waitFor(() => expect(split()).toBe(50));
  fireEvent.keyDown(separator(), { key: "Home" });
  await waitFor(() => expect(split()).toBeLessThan(50));
  expect(split()).toBeGreaterThanOrEqual(320 / width * 100);
  fireEvent.keyDown(separator(), { key: "Enter" });
  await waitFor(() => expect(split()).toBe(50));
});

test("closing and reopening preview restores the resized split without remounting either view", async () => {
  const view = render(content());
  await resize(width);
  expect(screen.getByRole("complementary", { name: "Preview" })).toBeDefined();
  fireEvent.click(screen.getByRole("button", { name: "Chat 0" }));
  fireEvent.click(screen.getByRole("button", { name: "Preview 0" }));
  fireEvent.keyDown(separator(), { key: "ArrowLeft" });
  await waitFor(() => expect(split()).toBeLessThan(50));
  const remembered = split();
  view.rerender(content(false));
  expect(screen.queryByRole("complementary", { name: "Preview" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Preview 1" })).toBeNull();
  expect(screen.queryByRole("separator", { name: "Resize workspace panel" })).toBeNull();
  expect(screen.getByRole("button", { name: "Chat 1" })).toBeDefined();
  view.rerender(content(true));
  await waitFor(() => expect(split()).toBe(remembered));
  expect(screen.getByRole("button", { name: "Chat 1" })).toBeDefined();
  expect(screen.getByRole("button", { name: "Preview 1" })).toBeDefined();
});

test("compact layout shows one view at a time and restores the desktop split after growing", async () => {
  const view = render(content());
  await resize(width);
  fireEvent.keyDown(separator(), { key: "ArrowLeft" });
  await waitFor(() => expect(split()).toBeLessThan(50));
  const remembered = split();
  await resize(600);
  expect(screen.queryByRole("main", { name: "Chat" })).toBeNull();
  expect(screen.getByRole("button", { name: "Preview 0" })).toBeDefined();
  view.rerender(content(false));
  expect(screen.getByRole("button", { name: "Chat 0" })).toBeDefined();
  expect(screen.queryByRole("button", { name: "Preview 0" })).toBeNull();
  view.rerender(content(true));
  await resize(1200);
  await waitFor(() => expect(split()).toBe(remembered));
  expect(screen.getByRole("main", { name: "Chat" })).toBeDefined();
});

test("pointer resizing continues over a preview frame and restores frame interaction on release", async () => {
  render(<><style data-layout-styles>{layoutCss}</style><WorkspaceLayout previewVisible preview={<iframe title="Embedded preview" />}><main>Chat</main></WorkspaceLayout></>);
  await resize(width);
  const frame = screen.getByTitle("Embedded preview");
  const initialPointerEvents = getComputedStyle(frame).pointerEvents;
  const handle = separator();
  const initial = split();
  fireEvent.pointerMove(handle, { pointerId: 1, pointerType: "mouse", isPrimary: true, clientX: width / 2, clientY: 50 });
  fireEvent.pointerDown(handle, { button: 0, buttons: 1, pointerId: 1, pointerType: "mouse", isPrimary: true, clientX: width / 2, clientY: 50 });
  await waitFor(() => expect(handle.getAttribute("data-separator")).toBe("active"));
  expect(getComputedStyle(frame).pointerEvents).toBe("none");
  fireEvent.pointerMove(document, { buttons: 1, pointerId: 1, pointerType: "mouse", isPrimary: true, clientX: width / 2 - 100, clientY: 50 });
  await waitFor(() => expect(split()).toBeLessThan(initial));
  fireEvent.pointerUp(document, { pointerId: 1, pointerType: "mouse", isPrimary: true, clientX: width / 2 - 100, clientY: 50 });
  await waitFor(() => expect(handle.getAttribute("data-separator")).not.toBe("active"));
  expect(getComputedStyle(frame).pointerEvents).toBe(initialPointerEvents);
});

test("both sides stay within the supported quarter-to-three-quarter split in a wide window", async () => {
  render(content());
  await resize(2000);
  fireEvent.keyDown(separator(), { key: "End" });
  await waitFor(() => expect(split()).toBeGreaterThan(50));
  expect(split()).toBeLessThanOrEqual(75);
  fireEvent.keyDown(separator(), { key: "Home" });
  await waitFor(() => expect(split()).toBeLessThan(50));
  expect(split()).toBeGreaterThanOrEqual(25);
});

test("external resizing and panel toggles do not replace the user's requested split with a temporary minimum", async () => {
  const view = render(content());
  await resize(2000);
  fireEvent.keyDown(separator(), { key: "Home" });
  await waitFor(() => expect(split()).toBe(25));
  await resize(900);
  expect(split()).toBeGreaterThan(25);
  await resize(2000);
  await waitFor(() => expect(split()).toBe(25));
  await resize(900);
  expect(split()).toBeGreaterThan(25);
  view.rerender(content(false));
  view.rerender(content(true));
  await resize(2000);
  await waitFor(() => expect(split()).toBe(25));
});


test("preview opening and closing use intermediate widths without remounting its content", async () => {
  const view = render(content(false)); await resize(width);
  const panel = document.getElementById("preview")!;
  const size = () => Number(panel.style.flexGrow);
  const subtree = panel.firstElementChild;
  expect(size()).toBe(0);
  view.rerender(content(true));
  await waitFor(() => { expect(size()).toBeGreaterThan(0); expect(size()).toBeLessThan(50); });
  await waitFor(() => expect(size()).toBe(50));
  view.rerender(content(false));
  await waitFor(() => { expect(size()).toBeGreaterThan(0); expect(size()).toBeLessThan(50); });
  await waitFor(() => expect(size()).toBe(0));
  expect(panel.firstElementChild).toBe(subtree);
});

test("reducing motion during preview opening settles immediately and rapid toggles preserve the target", async () => {
  const listeners = new Set<() => void>();
  const media = { matches: false, addEventListener: (_: string, fn: () => void) => listeners.add(fn), removeEventListener: (_: string, fn: () => void) => listeners.delete(fn) };
  vi.stubGlobal("matchMedia", () => media);
  const view = render(content(false)); await resize(width);
  const panel = document.getElementById("preview")!;
  const size = () => Number(panel.style.flexGrow);
  view.rerender(content(true));
  await waitFor(() => { expect(size()).toBeGreaterThan(0); expect(size()).toBeLessThan(50); });
  await act(async () => { media.matches = true; listeners.forEach(fn => fn()); });
  await waitFor(() => expect(size()).toBe(50));
  view.rerender(content(false)); expect(size()).toBe(0);
  view.rerender(content(true)); expect(size()).toBe(50);
});


test("reversing a preview transition settles at the most recent requested visibility", async () => {
  const view = render(content(false)); await resize(width);
  const panel = document.getElementById("preview")!;
  const size = () => Number(panel.style.flexGrow);
  view.rerender(content(true));
  await waitFor(() => { expect(size()).toBeGreaterThan(0); expect(size()).toBeLessThan(50); });
  view.rerender(content(false));
  view.rerender(content(true));
  await waitFor(() => expect(size()).toBe(50));
  expect(screen.getByRole("button", { name: "Preview 0" })).toBeTruthy();
});


test("reduced-motion opening immediately restores keyboard resizing", async () => {
  vi.stubGlobal("matchMedia", () => ({ matches: true, addEventListener() {}, removeEventListener() {} }));
  const view = render(content(false)); await resize(width);
  view.rerender(content(true));
  expect(split()).toBe(50);
  fireEvent.keyDown(separator(), { key: "ArrowLeft" });
  await waitFor(() => expect(split()).toBeLessThan(50));
});
