import { expect, test, vi } from "vitest";
import type { AgentRunSummary } from "../src/lib/chatBridge";
import { readSessionActivity } from "../src/components/app/sessionActivity";
const run = (sessionId: string, status: string, unread = false): AgentRunSummary => ({ agentRunId: sessionId + status, sessionId, turnId: sessionId, status, unread, startedAtMs: 1, updatedAtMs: 2 });
const sessions = ["current", "waiting", "finished", "failed"].map(id => ({ id, sessionKind: "main" as const }));
test("Runtime facts provide background completion and questions without a transcript subscription", async () => {
  const listRuns = vi.fn(async () => [run("current", "running"), run("waiting", "running"), run("finished", "succeeded", true), run("failed", "failed", true), run("child", "running")]);
  const readQuestionCount = vi.fn(async (id: string) => id === "waiting" ? 1 : 0);
  const result = await readSessionActivity(sessions, "current", { listRuns, readQuestionCount });
  expect(listRuns).toHaveBeenCalledOnce();
  expect(readQuestionCount).toHaveBeenCalledTimes(4);
  expect(readQuestionCount).toHaveBeenCalledWith("waiting");
  expect([...result!.runningSessionIds].sort()).toEqual(["current", "waiting"]);
  expect([...result!.completedSessionIds].sort()).toEqual(["failed", "finished"]);
  expect([...result!.waitingSessionIds]).toEqual(["waiting"]);
});
test("question discovery has at most two host requests in flight and preserves failed observations", async () => {
  const scope = ["a", "b", "c", "d"].map(id => ({ id, sessionKind: "main" as const }));
  let active = 0, peak = 0;
  const readQuestionCount = vi.fn(async (id: string) => { active++; peak = Math.max(active, peak); await Promise.resolve(); active--; if (id === "b") throw new Error("offline"); return id === "c" ? 1 : 0; });
  const result = await readSessionActivity(scope, null, { listRuns: async () => scope.map(item => run(item.id, "running")), readQuestionCount });
  expect(peak).toBe(2);
  expect(readQuestionCount).toHaveBeenCalledTimes(4);
  expect(result!.checkedWaitingSessionIds.has("b")).toBe(false);
  expect([...result!.waitingSessionIds]).toEqual(["c"]);
});
test("a superseded discovery stops starting host work and cannot publish a stale snapshot", async () => {
  let current = true;
  const readQuestionCount = vi.fn(async () => 0);
  const result = await readSessionActivity(sessions, null, { listRuns: async () => { current = false; return [run("waiting", "running")]; }, readQuestionCount }, () => current);
  expect(result).toBeNull();
  expect(readQuestionCount).not.toHaveBeenCalled();
});

test("an unanswered checkpoint retains attention after its run has ended", async () => {
  const result = await readSessionActivity(sessions, "current", {
    listRuns: async () => [run("waiting", "succeeded")],
    readQuestionCount: async () => 1,
  }, () => true, new Set(["waiting"]));
  expect(result!.runningSessionIds.size).toBe(0);
  expect(result!.waitingSessionIds.has("waiting")).toBe(true);
});
