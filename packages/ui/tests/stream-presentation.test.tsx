import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { useStreamPresentation } from "../src/useStreamPresentation";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const originalDescriptors = new Map(
  ["window", "document", "requestAnimationFrame", "cancelAnimationFrame", "matchMedia"].map(
    (name) => [name, Object.getOwnPropertyDescriptor(globalThis, name)] as const,
  ),
);

let frameNow = 0;
let nextFrameId = 1;
let reducedMotion = false;
let scheduledFrames = new Map<number, FrameRequestCallback>();
let performanceNow: ReturnType<typeof vi.spyOn> | null = null;
let scheduledMotionFrames = new WeakMap<(data: { timestamp: number }) => void, number>();
// Control the external frame scheduler; the text and lifecycle assertions stay real.
vi.mock("motion", () => ({
  frame: { update(callback: (data: { timestamp: number }) => void) {
    const id = requestFrame(timestamp => callback({ timestamp }));
    scheduledMotionFrames.set(callback, id);
    return callback;
  } },
  cancelFrame(callback: (data: { timestamp: number }) => void) {
    const id = scheduledMotionFrames.get(callback);
    if (id !== undefined) cancelFrame(id);
    scheduledMotionFrames.delete(callback);
  },
}));

const requestFrame = (callback: FrameRequestCallback): number => {
  const frameId = nextFrameId++;
  scheduledFrames.set(frameId, callback);
  return frameId;
};

const cancelFrame = (frameId: number): void => {
  scheduledFrames.delete(frameId);
};

const mediaQuery = (): MediaQueryList =>
  ({ matches: reducedMotion }) as MediaQueryList;

const runNextFrame = async (at: number): Promise<void> => {
  const entry = scheduledFrames.entries().next().value as
    | [number, FrameRequestCallback]
    | undefined;
  if (!entry) {
    throw new Error("stream presentation did not schedule a frame");
  }
  scheduledFrames.delete(entry[0]);
  frameNow = at;
  await act(async () => entry[1](at));
};

beforeEach(() => {
  frameNow = 0;
  nextFrameId = 1;
  reducedMotion = false;
  scheduledFrames = new Map();
  scheduledMotionFrames = new WeakMap();
  performanceNow = vi.spyOn(globalThis.performance, "now").mockImplementation(
    () => frameNow,
  );
  const documentValue = {
    hidden: false,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  };
  const windowValue = {
    requestAnimationFrame: requestFrame,
    cancelAnimationFrame: cancelFrame,
    matchMedia: mediaQuery,
  };
  Object.defineProperties(globalThis, {
    window: { configurable: true, value: windowValue },
    document: { configurable: true, value: documentValue },
    requestAnimationFrame: { configurable: true, value: requestFrame },
    cancelAnimationFrame: { configurable: true, value: cancelFrame },
    matchMedia: { configurable: true, value: mediaQuery },
  });
});

afterEach(() => {
  performanceNow?.mockRestore();
  performanceNow = null;
  for (const [name, descriptor] of originalDescriptors) {
    if (descriptor) {
      Object.defineProperty(globalThis, name, descriptor);
    } else {
      Reflect.deleteProperty(globalThis, name);
    }
  }
});

test("reveals a backlogged live answer on every 60 Hz frame without large jumps", async () => {
  const target = "x".repeat(400);
  let visible = "";
  const Harness = ({ text, live }: { text: string; live: boolean }) => {
    visible = useStreamPresentation(text, live);
    return <span>{visible}</span>;
  };
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<Harness text="" live />);
  });
  await act(async () => {
    renderer?.update(<Harness text={target} live />);
  });

  const samples: string[] = [];
  for (let frame = 1; frame <= 6; frame += 1) {
    await runNextFrame((frame * 1_000) / 60);
    samples.push(visible);
  }

  expect(new Set(samples).size).toBe(samples.length);
  let previous = "";
  for (const sample of samples) {
    expect(target.startsWith(sample)).toBe(true);
    expect(sample.length - previous.length).toBeGreaterThanOrEqual(1);
    expect(sample.length - previous.length).toBeLessThanOrEqual(12);
    previous = sample;
  }

  await act(async () => renderer?.unmount());
});

test("drains an append-only terminal answer instead of revealing the final batch at once", async () => {
  const target = "terminal ".repeat(80);
  let visible = "";
  const samples: string[] = [];
  const Harness = ({ live }: { live: boolean }) => {
    visible = useStreamPresentation(target, live);
    return <span>{visible}</span>;
  };
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<Harness live />);
  });
  await runNextFrame(1_000 / 60);
  const beforeTerminal = visible;

  await act(async () => {
    renderer?.update(<Harness live={false} />);
  });
  expect(visible).toBe(beforeTerminal);

  for (let frame = 2; frame <= 14; frame += 1) {
    await runNextFrame((frame * 1_000) / 60);
    samples.push(visible);
  }
  expect(samples.filter((sample) => sample !== target).length)
    .toBeGreaterThanOrEqual(3);
  expect(visible).toBe(target);

  await act(async () => renderer?.unmount());
});

test("applies corrections and reduced-motion content immediately", async () => {
  let visible = "";
  const Harness = ({ text }: { text: string }) => {
    visible = useStreamPresentation(text, true);
    return <span>{visible}</span>;
  };
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<Harness text={"append-only draft ".repeat(20)} />);
  });
  await runNextFrame(1_000 / 60);
  await act(async () => {
    renderer?.update(<Harness text="corrected answer" />);
  });
  expect(visible).toBe("corrected answer");
  await act(async () => renderer?.unmount());

  reducedMotion = true;
  await act(async () => {
    renderer = create(<Harness text="complete without animation" />);
  });
  expect(visible).toBe("complete without animation");
  await act(async () => renderer?.unmount());
});

test("stream presentation preserves complete graphemes and cancels pending work on unmount", async () => {
  const grapheme = "👩‍💻";
  const target = grapheme.repeat(40);
  let visible = "";
  function Harness() { visible = useStreamPresentation(target, true); return <span>{visible}</span>; }
  let renderer: ReactTestRenderer;
  await act(async () => { renderer = create(<Harness />); });
  expect(visible).toBe(grapheme);
  for (let step = 1; step <= 3; step++) {
    await runNextFrame(step * 1000 / 60);
    expect(visible.length % grapheme.length).toBe(0);
    expect(target.startsWith(visible)).toBe(true);
  }
  expect(scheduledFrames.size).toBeGreaterThan(0);
  await act(async () => renderer.unmount());
  expect(scheduledFrames.size).toBe(0);
});

test("hiding the document flushes complete authoritative text and clears pending presentation", async () => {
  const target = "complete hidden answer ".repeat(50);
  let visible = "";
  function Harness() { visible = useStreamPresentation(target, true); return <span>{visible}</span>; }
  let renderer: ReactTestRenderer;
  await act(async () => { renderer = create(<Harness />); });
  expect(visible).not.toBe(target);
  const listener = vi.mocked(document.addEventListener).mock.calls.find(([event]) => event === "visibilitychange")?.[1];
  expect(listener).toBeTypeOf("function");
  await act(async () => {
    Object.defineProperty(document, "hidden", { configurable: true, value: true });
    (listener as () => void)();
  });
  expect(visible).toBe(target);
  expect(scheduledFrames.size).toBe(0);
  await act(async () => renderer.unmount());
});
