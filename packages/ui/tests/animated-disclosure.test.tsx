import { useEffect } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { AnimatedDisclosure } from "../src/components/chat/AnimatedDisclosure";

type Playback = { finish(): void; cancel: ReturnType<typeof vi.fn> };
let renderer: ReactTestRenderer;
const motion = vi.hoisted(() => ({ animate: vi.fn() }));
vi.mock("motion/react", async () => {
  const { useRef } = await import("react");
  return { useAnimate: () => [useRef(null), motion.animate] };
});
let animations: Playback[];
let element: { scrollHeight: number; style: Record<string, string> };
let preference: { matches: boolean; change?: () => void; addEventListener: ReturnType<typeof vi.fn>; removeEventListener: ReturnType<typeof vi.fn> };

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  animations = [];
  motion.animate.mockImplementation(() => {
    let finish = () => {};
    const finished = new Promise<void>(resolve => { finish = resolve; });
    const playback = { finish, then: finished.then.bind(finished), stop: vi.fn() };
    animations.push({ finish, cancel: playback.stop });
    return playback;
  });
  preference = { matches: false,
    addEventListener: vi.fn((_event: string, listener: () => void) => { preference.change = listener; }),
    removeEventListener: vi.fn(),
  };
  vi.stubGlobal("window", { matchMedia: () => preference });
  element = { scrollHeight: 100, style: {} };
});
afterEach(() => { act(() => renderer?.unmount()); vi.unstubAllGlobals(); });

function view(expanded: boolean, animateHeight = true) {
  return <AnimatedDisclosure expanded={expanded} animateHeight={animateHeight}><p>Output</p></AnimatedDisclosure>;
}
function mount(expanded: boolean) {
  act(() => { renderer = create(view(expanded), { createNodeMock: () => element }); });
}

test("closed history stays unmounted and opening mounts output lazily", () => {
  mount(false);
  expect(renderer.root.findAllByType("p")).toHaveLength(0);
  expect(animations).toHaveLength(0);
  act(() => renderer.update(view(true)));
  expect(renderer.root.findAllByType("p")).toHaveLength(1);
  expect(renderer.root.findByType("div").props.inert).toBe(false);
});

test("expanded history is readable immediately without an entrance animation", () => {
  mount(true);
  expect(renderer.root.findAllByType("p")).toHaveLength(1);
  expect(renderer.root.findByType("div").props.hidden).toBe(false);
  expect(animations).toHaveLength(0);
});

test("closing content becomes inert immediately and releases readers after the last closure", async () => {
  const release = vi.fn();
  function Reader() { useEffect(() => release, []); return <p>Output</p>; }
  const readerView = (expanded: boolean) => <AnimatedDisclosure expanded={expanded}><Reader /></AnimatedDisclosure>;
  act(() => { renderer = create(readerView(true), { createNodeMock: () => element }); });
  act(() => renderer.update(readerView(false)));
  expect(renderer.root.findByType("div").props.inert).toBe(true);
  expect(renderer.root.findByType("div").props["aria-hidden"]).toBe(true);
  expect(renderer.root.findAllByType("p")).toHaveLength(1);
  expect(release).not.toHaveBeenCalled();
  await act(async () => animations[0].finish());
  expect(renderer.root.findAllByType("p")).toHaveLength(0);
  expect(release).toHaveBeenCalledOnce();
});

test("rapid reversal cannot release newly reopened content", async () => {
  mount(false);
  act(() => renderer.update(view(true)));
  act(() => renderer.update(view(false)));
  act(() => renderer.update(view(true)));
  await act(async () => animations[1].finish());
  expect(renderer.root.findAllByType("p")).toHaveLength(1);
  expect(animations[1].cancel).toHaveBeenCalledOnce();
  act(() => renderer.update(view(false)));
  await act(async () => animations[3].finish());
  expect(renderer.root.findAllByType("p")).toHaveLength(0);
});

test.each(["reduced motion", "long content", "height animation disabled"])("%s settles immediately without retaining hidden readers", mode => {
  preference.matches = mode === "reduced motion";
  if (mode === "long content") element.scrollHeight = 2000;
  mount(true);
  act(() => renderer.update(view(false, mode !== "height animation disabled")));
  expect(renderer.root.findAllByType("p")).toHaveLength(0);
  expect(renderer.root.findByType("div").props.hidden).toBe(true);
  expect(animations).toHaveLength(0);
});

test("unmount cancels in-flight disclosure work", () => {
  mount(false);
  act(() => renderer.update(view(true)));
  act(() => renderer.unmount());
  expect(animations[0].cancel).toHaveBeenCalledOnce();
});

test("switching to reduced motion finishes a closing interaction and frees its reader", () => {
  mount(true);
  act(() => renderer.update(view(false)));
  expect(renderer.root.findAllByType("p")).toHaveLength(1);
  preference.matches = true;
  act(() => preference.change?.());
  expect(renderer.root.findAllByType("p")).toHaveLength(0);
  expect(animations[0].cancel).toHaveBeenCalledOnce();
});

test("an opened disclosure returns to natural height for streaming output", async () => {
  mount(false);
  act(() => renderer.update(view(true)));
  await act(async () => animations[0].finish());
  expect(element.style.height).toBe("auto");
  expect(element.style.opacity).toBe("1");
  expect(preference.removeEventListener).toHaveBeenCalledWith("change", preference.change);
});

test("unmount removes the active system-preference listener", () => {
  mount(false);
  act(() => renderer.update(view(true)));
  act(() => renderer.unmount());
  expect(preference.removeEventListener).toHaveBeenCalledWith("change", preference.change);
});

test("a host without a DOM node still releases a closing reader immediately", () => {
  act(() => { renderer = create(view(true)); });
  expect(renderer.root.findAllByType("p")).toHaveLength(1);
  act(() => renderer.update(view(false)));
  expect(renderer.root.findAllByType("p")).toHaveLength(0);
  expect(animations).toHaveLength(0);
});
