// @vitest-environment jsdom
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { changeThemeAnimated } from "../src/theme";

type NativePlayback = {
  playState: string; currentTime: number; playbackRate: number; onfinish: (() => void) | null;
  effect: { getComputedTiming(): { duration: number } }; finish(): void;
  cancel: ReturnType<typeof vi.fn>; play(): void; pause(): void;
};
function deferred() {
  let resolve!: () => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<void>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function viewTransition() {
  const ready = deferred(), finished = deferred();
  return { ready: ready.promise, finished: finished.promise, updateCallbackDone: Promise.resolve(),
    skipTransition: vi.fn(), readyEvent: ready, finishEvent: finished };
}
let reduced = false;
let hidden = false;
let mediaListeners: Set<() => void>;
let transitions: ReturnType<typeof viewTransition>[];
let animations: NativePlayback[];
let start: ReturnType<typeof vi.fn>;
let animate: ReturnType<typeof vi.fn>;
const originalAnimate = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "animate");
const originalViewTransition = Object.getOwnPropertyDescriptor(document, "startViewTransition");

beforeEach(() => {
  reduced = false; hidden = false; mediaListeners = new Set(); transitions = []; animations = [];
  document.documentElement.dataset.themePreference = "system";
  vi.stubGlobal("matchMedia", () => ({ get matches() { return reduced; },
    addEventListener: (_event: string, listener: () => void) => mediaListeners.add(listener),
    removeEventListener: (_event: string, listener: () => void) => mediaListeners.delete(listener) }));
  vi.spyOn(document, "hidden", "get").mockImplementation(() => hidden);
  start = vi.fn((update: () => void) => {
    const transition = viewTransition(); transitions.push(transition); update(); return transition;
  });
  Object.defineProperty(document, "startViewTransition", { configurable: true, value: start });
  animate = vi.fn(() => {
    const playback: NativePlayback = { playState: "running", currentTime: 0, playbackRate: 1, onfinish: null,
      effect: { getComputedTiming: () => ({ duration: 520 }) },
      finish() { playback.playState = "finished"; playback.onfinish?.(); },
      cancel: vi.fn(() => { playback.playState = "idle"; }), play() {}, pause() {} };
    animations.push(playback); return playback;
  });
  Object.defineProperty(HTMLElement.prototype, "animate", { configurable: true, value: animate });
});
afterEach(async () => {
  for (const transition of transitions) { transition.readyEvent.resolve(); transition.finishEvent.resolve(); }
  await Promise.resolve(); await Promise.resolve();
  vi.restoreAllMocks(); vi.unstubAllGlobals();
  if (originalAnimate) Object.defineProperty(HTMLElement.prototype, "animate", originalAnimate);
  else Reflect.deleteProperty(HTMLElement.prototype, "animate");
  if (originalViewTransition) Object.defineProperty(document, "startViewTransition", originalViewTransition);
  else Reflect.deleteProperty(document, "startViewTransition");
});
async function ready(index = 0) {
  transitions[index].readyEvent.resolve(); await Promise.resolve(); await Promise.resolve();
}

test("the theme snapshot animates its new root layer around the activation point", async () => {
  changeThemeAnimated("dark", { x: 20, y: 30 });
  expect(document.documentElement.dataset.themePreference).toBe("dark");
  await ready();
  expect(animate).toHaveBeenCalledWith(expect.objectContaining({ clipPath: ["circle(0px at 20px 30px)", expect.stringContaining("at 20px 30px")] }),
    expect.objectContaining({ pseudoElement: "::view-transition-new(root)", duration: 520 }));
});

test.each(["reduced motion", "hidden document", "unsupported view transitions"])("%s applies the requested theme immediately", mode => {
  reduced = mode === "reduced motion"; hidden = mode === "hidden document";
  if (mode === "unsupported view transitions") Object.defineProperty(document, "startViewTransition", { configurable: true, value: undefined });
  changeThemeAnimated("light", { x: 20, y: 30 });
  expect(document.documentElement.dataset.themePreference).toBe("light");
  expect(start).not.toHaveBeenCalled(); expect(animate).not.toHaveBeenCalled();
});

test("a superseded snapshot cannot start a stale theme animation", async () => {
  changeThemeAnimated("dark", { x: 20, y: 30 });
  changeThemeAnimated("light", { x: 40, y: 50 });
  expect(transitions[0].skipTransition).toHaveBeenCalledOnce();
  await ready(0);
  expect(animate).not.toHaveBeenCalled();
  await ready(1);
  expect(animate).toHaveBeenCalledOnce();
  expect(document.documentElement.dataset.themePreference).toBe("light");
});

test("real Motion cancels the departed pseudo-element animation before a new theme snapshot", async () => {
  changeThemeAnimated("dark", { x: 20, y: 30 }); await ready();
  const previous = animations[0];
  changeThemeAnimated("light", { x: 40, y: 50 });
  expect(previous.cancel).toHaveBeenCalledOnce();
  expect(previous.playState).toBe("idle");
  expect(transitions[0].skipTransition).toHaveBeenCalledOnce();
  await ready(1);
  expect(animate).toHaveBeenCalledTimes(2);
  expect(document.documentElement.dataset.themePreference).toBe("light");
});

test("an immediate reduced-motion choice invalidates a pending snapshot update", async () => {
  let oldUpdate!: () => void;
  start.mockImplementationOnce((update: () => void) => {
    oldUpdate = update; const transition = viewTransition(); transitions.push(transition); return transition;
  });
  changeThemeAnimated("dark", { x: 20, y: 30 });
  reduced = true;
  changeThemeAnimated("light", { x: 40, y: 50 });
  oldUpdate(); await ready();
  expect(document.documentElement.dataset.themePreference).toBe("light");
  expect(transitions[0].skipTransition).toHaveBeenCalledOnce();
  expect(animate).not.toHaveBeenCalled();
});

test.each(["reduced motion", "hidden document"])("enabling %s cancels a running snapshot and removes event listeners", async mode => {
  changeThemeAnimated("dark", { x: 20, y: 30 }); await ready();
  if (mode === "reduced motion") {
    reduced = true; for (const listener of mediaListeners) listener();
  } else {
    hidden = true; document.dispatchEvent(new Event("visibilitychange"));
  }
  expect(animations[0].cancel).toHaveBeenCalledOnce();
  expect(transitions[0].skipTransition).toHaveBeenCalledOnce();
  expect(mediaListeners.size).toBe(0);
  expect(document.documentElement.dataset.themePreference).toBe("dark");
});

test("synchronous browser transition failure falls back to the requested theme", () => {
  start.mockImplementationOnce(() => { throw new Error("snapshot unsupported"); });
  expect(() => changeThemeAnimated("light", { x: 20, y: 30 })).not.toThrow();
  expect(document.documentElement.dataset.themePreference).toBe("light");
  expect(mediaListeners.size).toBe(0);
});

test("a failed snapshot applies its requested theme even if the browser did not run the update", async () => {
  start.mockImplementationOnce(() => {
    const transition = viewTransition(); transitions.push(transition); return transition;
  });
  changeThemeAnimated("light", { x: 20, y: 30 });
  transitions[0].readyEvent.reject(new Error("snapshot unavailable"));
  await Promise.resolve(); await Promise.resolve();
  expect(document.documentElement.dataset.themePreference).toBe("light");
  expect(transitions[0].skipTransition).toHaveBeenCalledOnce();
  expect(mediaListeners.size).toBe(0);
});

test("an old failure cannot override a newer requested theme", async () => {
  changeThemeAnimated("dark", { x: 20, y: 30 });
  changeThemeAnimated("light", { x: 40, y: 50 });
  transitions[0].readyEvent.reject(new Error("old snapshot unavailable"));
  await Promise.resolve(); await Promise.resolve();
  expect(document.documentElement.dataset.themePreference).toBe("light");
  await ready(1);
  expect(animate).toHaveBeenCalledOnce();
});

test("Motion playback failure keeps the theme and releases the browser snapshot", async () => {
  animate.mockImplementationOnce(() => { throw new Error("animation unsupported"); });
  changeThemeAnimated("dark", { x: 20, y: 30 }); await ready();
  expect(document.documentElement.dataset.themePreference).toBe("dark");
  expect(transitions[0].skipTransition).toHaveBeenCalledOnce();
  expect(mediaListeners.size).toBe(0);
});

test("finishing a browser snapshot releases Motion playback and its platform listeners", async () => {
  changeThemeAnimated("dark", { x: 20, y: 30 }); await ready();
  expect(mediaListeners.size).toBe(1);
  transitions[0].finishEvent.resolve(); await Promise.resolve();
  expect(mediaListeners.size).toBe(0);
  expect(animations[0].cancel).toHaveBeenCalledOnce();
  expect(transitions[0].skipTransition).not.toHaveBeenCalled();
});
