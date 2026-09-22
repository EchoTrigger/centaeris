import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { SessionItem } from "../src/lib/chatBridge";
import { useSessionController } from "../src/components/app/useSessionController";

const mocks = vi.hoisted(() => ({ list: vi.fn(), error: vi.fn() }));
vi.mock("../src/lib/chatBridge", () => ({ listSessions: mocks.list, activateSession: vi.fn(async () => {}), deleteSession: vi.fn(), updateSession: vi.fn() }));
vi.mock("../src/components/chat/chatRuntimeCore", () => ({ sessionViewCacheStore: new Map() }));
vi.mock("../src/lib/workspaceBridge", () => ({ activateWorkspaceRoot: vi.fn() }));

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
let controller: ReturnType<typeof useSessionController>;
let renderer: ReactTestRenderer | undefined;
let host: EventTarget;
let page: EventTarget & { visibilityState: string };
const session = (id: string): SessionItem => ({ id, cwd: "D:\\Current", title: id, updatedAt: 1, messageCount: 1, sessionKind: "main", activityState: "idle" });
function Harness() {
  controller = useSessionController({ activeWorkspaceRoot: "D:\\Current", reportError: mocks.error });
  return null;
}
beforeEach(async () => {
  vi.useFakeTimers(); mocks.list.mockReset(); mocks.error.mockReset();
  host = new EventTarget(); page = Object.assign(new EventTarget(), { visibilityState: "visible" });
  vi.stubGlobal("window", host); vi.stubGlobal("document", page);
  mocks.list.mockResolvedValue([session("current")]);
  await act(async () => { renderer = create(<Harness />); });
  await act(async () => { controller.actions.beginInitialization().applySessions([session("current")], "current"); });
});
afterEach(async () => {
  await act(async () => renderer?.unmount()); renderer = undefined;
  vi.useRealTimers(); vi.unstubAllGlobals();
});
test("discovers a TUI session without changing the selected conversation or a new draft", async () => {
  mocks.list.mockResolvedValue([session("tui"), session("current")]);
  await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
  expect(controller.mainSessions.map((item) => item.id)).toContain("tui");
  expect(controller.currentSessionId).toBe("current");
  await act(async () => controller.actions.clearSelection());
  await act(async () => { host.dispatchEvent(new Event("focus")); });
  expect(controller.currentSessionId).toBeNull();
});
test("pauses while hidden and refreshes on return without overlapping requests", async () => {
  page.visibilityState = "hidden";
  await act(async () => { await vi.advanceTimersByTimeAsync(10000); });
  expect(mocks.list).not.toHaveBeenCalled();
  let finish!: (items: SessionItem[]) => void;
  mocks.list.mockReturnValue(new Promise<SessionItem[]>((resolve) => { finish = resolve; }));
  page.visibilityState = "visible";
  await act(async () => { page.dispatchEvent(new Event("visibilitychange")); host.dispatchEvent(new Event("focus")); await vi.advanceTimersByTimeAsync(5000); });
  expect(mocks.list).toHaveBeenCalledTimes(1);
  await act(async () => { finish([session("tui")]); });
  expect(controller.mainSessions.map((item) => item.id)).toContain("tui");
});
test("a discovery response cannot erase a conversation just created locally", async () => {
  let finish!: (items: SessionItem[]) => void;
  mocks.list.mockReturnValue(new Promise<SessionItem[]>((resolve) => { finish = resolve; }));
  await act(async () => { host.dispatchEvent(new Event("focus")); });
  await act(async () => { controller.actions.resolveSession(session("local"), { activate: true }); });
  await act(async () => { finish([session("current")]); });
  expect(controller.mainSessions.map((item) => item.id)).toContain("local");
  expect(controller.currentSessionId).toBe("local");
});

test("unchanged discovery preserves list identity and unmount removes the refresh lifecycle", async () => {
  const previous = controller.sessions;
  await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
  expect(controller.sessions).toBe(previous);
  const calls = mocks.list.mock.calls.length;
  await act(async () => { renderer?.unmount(); renderer = undefined; });
  host.dispatchEvent(new Event("focus"));
  page.dispatchEvent(new Event("visibilitychange"));
  await vi.advanceTimersByTimeAsync(10000);
  expect(mocks.list).toHaveBeenCalledTimes(calls);
});
