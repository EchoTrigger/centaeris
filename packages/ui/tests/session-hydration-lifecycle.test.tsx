import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, expect, test, vi } from "vitest";
import { useSessionHydration } from "../src/components/chat/useSessionHydration";
import { sessionViewCacheStore } from "../src/components/chat/chatRuntimeCore";
import type { SessionViewCacheEntry } from "../src/lib/sessionViewCache";
import type { SessionViewSnapshot } from "../src/components/chat/types";

vi.mock("../src/components/chat/chatRuntimeModel", () => ({ buildSessionHydrationSnapshot: vi.fn() }));
vi.mock("../src/components/chat/chatAreaModel", () => ({ waitForNextPaint: async () => {} }));
vi.mock("../src/components/chat/chatRuntimeCore", () => ({
  formatExecutionError: String,
  sessionViewCacheStore: { delete: vi.fn() },
}));
let renderer: ReactTestRenderer;
afterEach(async () => { if (renderer) await act(async () => renderer.unmount()); vi.useRealTimers(); vi.clearAllMocks(); });

test("rerendering the same chat does not prepare or synchronize it again", async () => {
  const prepare = vi.fn(() => ({ kind: "preserved" as const }));
  function Harness() {
    useSessionHydration({ currentSessionId: "a", prepare: () => prepare(), applySnapshot: () => {}, refreshCachedSession: async () => {}, onError: () => {} });
    return null;
  }
  await act(async () => { renderer = create(<Harness />); });
  await act(async () => { renderer.update(<Harness />); });
  expect(prepare).toHaveBeenCalledTimes(1);
});

test("cached synchronization retries once, keeps content, and exposes an explicit retry", async () => {
  vi.useFakeTimers();
  const refresh = vi.fn().mockRejectedValue(new Error("request timed out"));
  const fatal = vi.fn();
  let state: ReturnType<typeof useSessionHydration>;
  function Harness() {
    state = useSessionHydration({ currentSessionId: "a", prepare: () => ({ kind: "cached", entry: {} as SessionViewCacheEntry<SessionViewSnapshot> }), applySnapshot: () => {}, refreshCachedSession: refresh, onError: fatal });
    return null;
  }
  await act(async () => { renderer = create(<Harness />); });
  expect(state!.syncError).toBe("");
  await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
  expect(refresh).toHaveBeenCalledTimes(2);
  expect(state!.syncError).toContain("timed out");
  expect(fatal).not.toHaveBeenCalled();
  expect(sessionViewCacheStore.delete).not.toHaveBeenCalled();
  refresh.mockResolvedValue(undefined);
  await act(async () => state!.retrySessionSync());
  expect(state!.syncError).toBe("");
  expect(refresh).toHaveBeenCalledTimes(3);
});

test("a late cached failure cannot report an error for the next chat", async () => {
  let reject: (error: Error) => void = () => {};
  let state: ReturnType<typeof useSessionHydration>;
  const fatal = vi.fn();
  function Harness({ id }: { id: string }) {
    state = useSessionHydration({ currentSessionId: id, prepare: () => id === "a" ? ({ kind: "cached", entry: {} as SessionViewCacheEntry<SessionViewSnapshot> }) : ({ kind: "preserved" }), applySnapshot: () => {}, refreshCachedSession: () => new Promise<void>((_, fail) => { reject = fail; }), onError: fatal });
    return null;
  }
  await act(async () => { renderer = create(<Harness id="a" />); });
  await act(async () => { renderer.update(<Harness id="b" />); });
  await act(async () => reject(new Error("late failure")));
  expect(fatal).not.toHaveBeenCalled();
  expect(state!.syncError).toBe("");
});
