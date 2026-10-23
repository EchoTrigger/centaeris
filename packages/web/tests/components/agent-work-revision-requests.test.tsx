// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { useAgentWorkSessions } from "../../src/agent-chat/useAgentWorkSessions";
import { apiJson } from "../../src/api";

vi.mock("../../src/api", () => ({ apiJson: vi.fn(), ApiError: class extends Error { status = 0; } }));
const request = vi.mocked(apiJson);
const session = (id: string) => ({ id, currentEnvironmentId: "environment", executionMode: "perRun", workspaceId: "workspace", agentId: "agent",
  projectId: null, title: id, origin: "automation", status: "active", deletedAt: null, isPinned: false, isUnread: false,
  isManuallyUnread: false, latestAttentionSequence: "0", readThroughSequence: "0", attentionRevision: "0",
  hasActiveAgentRun: false, updatedAt: "2026-10-05T00:00:00Z", initialInputOrigin: null });
const page = (id = "work") => ({ schema: "agent.work_sessions.v1", agentId: "agent", workspaceId: "workspace",
  sessions: [session(id)], nextAfterSessionId: null, hasMore: false });
function View({ revision = "0", enabled = true }: { revision?: string; enabled?: boolean }) {
  const value = useAgentWorkSessions("agent", "workspace", enabled, () => {}, revision);
  return <div><span>{value.sessions.map(item => item.sessionId).join(",")}</span><span>{value.busy ? "busy" : "idle"}</span>
    <button type="button" onClick={value.onRetry}>Retry</button><button type="button" onClick={value.onLoadMore}>More</button></div>;
}
const settle = async () => { await act(async () => { await Promise.resolve(); }); };
beforeEach(() => {
  vi.useFakeTimers(); vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
  request.mockResolvedValue(page());
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.clearAllMocks(); vi.useRealTimers(); });
test("an open work overview does not reread its window while idle", async () => {
  render(<View />); await settle();
  await act(async () => { await vi.advanceTimersByTimeAsync(60_000); });
  expect(request).toHaveBeenCalledTimes(1);
});
test("work revisions coalesce during a slow request without replacing loaded rows with loading", async () => {
  let finish!: (value: unknown) => void;
  const view = render(<View />); await settle();
  request.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  view.rerender(<View revision="1" />); await settle();
  expect(screen.getByText("work")).toBeTruthy();
  expect(screen.getByText("busy")).toBeTruthy();
  view.rerender(<View revision="2" />); view.rerender(<View revision="3" />);
  expect(request).toHaveBeenCalledTimes(2);
  await act(async () => { finish(page("latest")); }); await settle();
  expect(request).toHaveBeenCalledTimes(3);
  view.rerender(<View revision="3" />); await settle();
  expect(request).toHaveBeenCalledTimes(3);
});
test("work reads pause when hidden or closed and calibrate once on becoming visible", async () => {
  const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
  const view = render(<View />); await settle(); expect(request).not.toHaveBeenCalled();
  visibility.mockReturnValue("visible");
  await act(async () => { document.dispatchEvent(new Event("visibilitychange")); });
  expect(request).toHaveBeenCalledTimes(1);
  view.rerender(<View enabled={false} revision="1" />); await settle();
  visibility.mockReturnValue("hidden"); document.dispatchEvent(new Event("visibilitychange"));
  visibility.mockReturnValue("visible"); document.dispatchEvent(new Event("visibilitychange"));
  await settle(); expect(request).toHaveBeenCalledTimes(1);
});
test("failed work reads wait for a new revision or explicit retry", async () => {
  request.mockRejectedValueOnce(new Error("offline"));
  render(<View />); await settle();
  await act(async () => { await vi.advanceTimersByTimeAsync(60_000); });
  expect(request).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getByRole("button", { name: "Retry" })); await settle();
  expect(request).toHaveBeenCalledTimes(2);
});

test("a work revision during explicit pagination waits and then revalidates the completed window", async () => {
  let finish!: (value: unknown) => void;
  let secondSignal: AbortSignal | undefined;
  const first = { ...page("one"), hasMore: true, nextAfterSessionId: "one" };
  let secondReads = 0;
  request.mockImplementation(async (path, options) => {
    if (String(path).includes("afterSessionId=")) {
      if (++secondReads === 1) {
        secondSignal = options?.signal ?? undefined;
        return new Promise(resolve => { finish = resolve; });
      }
      return page("two");
    }
    return first;
  });
  const view = render(<View />); await settle();
  fireEvent.click(screen.getByRole("button", { name: "More" })); await settle();
  view.rerender(<View revision="1" />); await settle();
  expect(secondSignal?.aborted).toBe(false);
  expect(request).toHaveBeenCalledTimes(2);
  await act(async () => { finish(page("two")); }); await settle();
  expect(request).toHaveBeenCalledTimes(4);
  expect(screen.getByText("one,two")).toBeTruthy();
});

test("closing a work overview cancels a pending read and reopening does not wait for that old response", async () => {
  let finish!: (value: unknown) => void;
  let oldSignal: AbortSignal | undefined;
  request.mockImplementationOnce((_path, options) => {
    oldSignal = options?.signal ?? undefined;
    return new Promise(resolve => { finish = resolve; });
  });
  const view = render(<View />); await settle();
  view.rerender(<View enabled={false} />); await settle();
  expect(oldSignal?.aborted).toBe(true);
  expect(screen.getByText("idle")).toBeTruthy();
  request.mockResolvedValue(page("reopened"));
  view.rerender(<View />); await settle();
  expect(request).toHaveBeenCalledTimes(2);
  expect(screen.getByText("reopened")).toBeTruthy();
  await act(async () => { finish(page("stale")); }); await settle();
  expect(screen.queryByText("stale")).toBe(null);
});
