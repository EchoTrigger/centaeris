import { afterEach, expect, test, vi } from "vitest";
import { listenHost } from "../src/host/hostBridge";
import { observeBackgroundRuns } from "../src/components/chat/backgroundRunObservation";
vi.mock("../src/host/hostBridge", () => ({ listenHost: vi.fn() }));
afterEach(() => vi.useRealTimers());

test("a completed unknown run is consumed after successful synchronization", async () => {
  vi.useFakeTimers();
  let receive: (event: { sessionId: string; agentRunId: string }) => void = () => {};
  vi.mocked(listenHost).mockImplementation(async (_name, handler) => { receive = handler; return () => {}; });
  const recover = vi.fn(async () => {});
  const stop = observeBackgroundRuns({ sessionId: "s", isKnown: () => false, canObserve: () => true, recover, onError: vi.fn() });
  receive({ sessionId: "s", agentRunId: "completed" });
  await vi.advanceTimersByTimeAsync(3000);
  expect(recover).toHaveBeenCalledTimes(1);
  stop();
});

test("transient synchronization failures retry once without flashing an error", async () => {
  vi.useFakeTimers();
  let receive: (event: { sessionId: string; agentRunId: string }) => void = () => {};
  vi.mocked(listenHost).mockImplementation(async (_name, handler) => { receive = handler; return () => {}; });
  const recover = vi.fn(async () => { throw new Error("request timed out"); });
  const onError = vi.fn();
  const stop = observeBackgroundRuns({ sessionId: "s", isKnown: () => false, canObserve: () => true, recover, onError });
  receive({ sessionId: "s", agentRunId: "new" });
  await vi.advanceTimersByTimeAsync(0);
  expect(onError).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(3000);
  expect(recover).toHaveBeenCalledTimes(2);
  expect(onError).toHaveBeenCalledTimes(1);
  stop();
});
test("unknown same-session runs hydrate once; another workspace and known runs do not", async () => {
  vi.useFakeTimers();
  let receive: (event: { sessionId: string; agentRunId: string }) => void = () => {};
  const unsubscribe = vi.fn<() => void>();
  vi.mocked(listenHost).mockImplementation(async (_name, handler) => { receive = handler; return unsubscribe; });
  const known = new Set(["old"]);
  let idle = false;
  const recover = vi.fn(async () => { known.add("new"); });
  const stop = observeBackgroundRuns({ sessionId: "s", isKnown: (id) => known.has(id), canObserve: () => idle, recover, onError: vi.fn() });
  receive({ sessionId: "other", agentRunId: "other-run" });
  receive({ sessionId: "s", agentRunId: "old" });
  receive({ sessionId: "s", agentRunId: "new" });
  await vi.advanceTimersByTimeAsync(0);
  expect(recover).not.toHaveBeenCalled();
  idle = true;
  await vi.advanceTimersByTimeAsync(250);
  expect(recover).toHaveBeenCalledTimes(1);
  receive({ sessionId: "s", agentRunId: "new" });
  await vi.advanceTimersByTimeAsync(1000);
  expect(recover).toHaveBeenCalledTimes(1);
  stop(); expect(unsubscribe).toHaveBeenCalledTimes(1);
});
test("workspace switch invalidates an in-flight hydration", async () => {
  vi.useFakeTimers();
  let receive: (event: { sessionId: string; agentRunId: string }) => void = () => {};
  vi.mocked(listenHost).mockImplementation(async (_name, handler) => { receive = handler; return () => {}; });
  let current = () => true;
  let resolve: (() => void) | undefined;
  const stop = observeBackgroundRuns({ sessionId: "s", isKnown: () => false, canObserve: () => true, recover: async (guard) => { current = guard; await new Promise<void>((done) => { resolve = done; }); }, onError: vi.fn() });
  receive({ sessionId: "s", agentRunId: "new" });
  await vi.advanceTimersByTimeAsync(0);
  expect(current()).toBe(true);
  stop(); expect(current()).toBe(false);
  resolve?.();
});

test("a notification arriving during synchronization is not consumed by the earlier read", async () => {
  vi.useFakeTimers();
  let receive: (event: { sessionId: string; agentRunId: string }) => void = () => {};
  vi.mocked(listenHost).mockImplementation(async (_name, handler) => { receive = handler; return () => {}; });
  let finish: () => void = () => {};
  const recover = vi.fn().mockImplementationOnce(() => new Promise<void>((resolve) => { finish = resolve; })).mockResolvedValue(undefined);
  const stop = observeBackgroundRuns({ sessionId: "s", isKnown: () => false, canObserve: () => true, recover, onError: vi.fn() });
  receive({ sessionId: "s", agentRunId: "new" });
  await vi.advanceTimersByTimeAsync(0);
  receive({ sessionId: "s", agentRunId: "new" });
  finish();
  await vi.advanceTimersByTimeAsync(1500);
  expect(recover).toHaveBeenCalledTimes(2);
  stop();
});
