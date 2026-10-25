// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { SessionItem } from "../src/lib/chatBridge";
const h = vi.hoisted(() => ({ items: [] as SessionItem[], sync: vi.fn(), list: vi.fn(), state: vi.fn() }));
vi.mock("../src/lib/sessionCatalogClient", () => ({ SessionCatalogClient: class { sync = h.sync; hasMore = () => false; } }));
vi.mock("../src/lib/chatBridge", () => ({
  listAgentRuns: h.list, getAgentState: h.state, activateSession: vi.fn(), deleteSession: vi.fn(),
  querySessionCatalog: vi.fn(), reorderSessions: vi.fn(), updateSession: vi.fn(),
}));
import { useSessionController } from "../src/components/app/useSessionController";
const reportError = vi.fn();
beforeEach(() => {
  vi.useFakeTimers();
  vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
  h.items = ["background", "finished"].map(id => ({ id, title: id, cwd: "D:/test", sessionKind: "main", activityState: "idle", messageCount: 0, updatedAt: 1, isPinned: false, isUnread: false }));
  h.sync.mockReset().mockImplementation(async () => h.items);
  h.list.mockReset().mockResolvedValue({ agentRuns: [
    { agentRunId: "r1", sessionId: "background", turnId: "t1", status: "running", unread: false, startedAtMs: 1, updatedAtMs: 2 },
    { agentRunId: "r2", sessionId: "finished", turnId: "t2", status: "succeeded", unread: true, startedAtMs: 1, updatedAtMs: 2 },
  ] });
  h.state.mockReset().mockImplementation(async (id: string) => ({ session_id: id, pending_question_count: id === "background" ? 1 : 0, pending_questions: [] }));
  reportError.mockClear();
});
afterEach(() => { cleanup(); vi.useRealTimers(); vi.restoreAllMocks(); });
test("the existing catalog cycle observes background attention without overlapping discovery", async () => {
  let resolveState!: (value: { pending_question_count: number }) => void;
  h.state.mockReturnValueOnce(new Promise(resolve => { resolveState = resolve; }));
  const view = renderHook(() => useSessionController({ activeWorkspaceRoot: null, reportError }));
  await act(async () => { view.result.current.actions.beginInitialization().applySessions(h.items); });
  await act(async () => { await vi.advanceTimersByTimeAsync(5_000); });
  expect(h.state).toHaveBeenCalledWith("background", true);
  await act(async () => { await vi.advanceTimersByTimeAsync(5_000); });
  expect(h.list).toHaveBeenCalledOnce();
  await act(async () => { resolveState({ pending_question_count: 1 }); });
  expect(view.result.current.runningSessionIds.has("background")).toBe(true);
  expect(view.result.current.waitingSessionIds.has("background")).toBe(true);
  expect(view.result.current.completedSessionIds.has("finished")).toBe(true);
  expect(reportError).not.toHaveBeenCalled();
});
test.each([true, false])("a late discovery cannot restore a question cleared by a newer event; prior indicator %s", async initialWaiting => {
  let resolveState!: (value: { pending_question_count: number }) => void;
  h.state.mockReturnValueOnce(new Promise(resolve => { resolveState = resolve; }));
  const view = renderHook(() => useSessionController({ activeWorkspaceRoot: null, reportError }));
  await act(async () => { view.result.current.actions.beginInitialization().applySessions(h.items); if (initialWaiting) view.result.current.actions.setWaiting("background", true); });
  await act(async () => { await vi.advanceTimersByTimeAsync(5_000); });
  await act(async () => { view.result.current.actions.setWaiting("background", false); });
  await act(async () => { resolveState({ pending_question_count: 1 }); });
  expect(view.result.current.waitingSessionIds.has("background")).toBe(false);
  expect(reportError).not.toHaveBeenCalled();
});

test("Runtime reconciliation retains locally observed unread completion until the user opens it", async () => {
  h.list.mockResolvedValue({ agentRuns: [] });
  const view = renderHook(() => useSessionController({ activeWorkspaceRoot: null, reportError }));
  await act(async () => { view.result.current.actions.beginInitialization().applySessions(h.items); });
  await act(async () => { view.result.current.actions.completeSession("finished"); });
  expect(view.result.current.completedSessionIds.has("finished")).toBe(true);
  h.sync.mockResolvedValueOnce([]); // A paged catalog is not a deletion or read acknowledgement.
  await act(async () => { await vi.advanceTimersByTimeAsync(5_000); });
  expect(view.result.current.completedSessionIds.has("finished")).toBe(true);
});

test("an observed background run completing creates attention even without a detached viewer marker", async () => {
  const view = renderHook(() => useSessionController({ activeWorkspaceRoot: null, reportError }));
  await act(async () => { view.result.current.actions.beginInitialization().applySessions(h.items); });
  await act(async () => { await vi.advanceTimersByTimeAsync(5_000); });
  expect(view.result.current.runningSessionIds.has("background")).toBe(true);
  h.list.mockResolvedValue({ agentRuns: [{ agentRunId: "r1", sessionId: "background", turnId: "t1", status: "succeeded", unread: false, startedAtMs: 1, updatedAtMs: 3 }] });
  await act(async () => { await vi.advanceTimersByTimeAsync(5_000); });
  expect(view.result.current.completedSessionIds.has("background")).toBe(true);
});
