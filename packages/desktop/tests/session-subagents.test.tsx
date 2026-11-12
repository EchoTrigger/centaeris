// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { AgentSessionTags } from "../src/renderer/components/chat/AgentSessionTags";
import { useSessionSubagents } from "../src/renderer/components/chat/useSessionSubagents";
import { restoreSessionSubagents, mergeSessionSubagents } from "../src/renderer/components/chat/sessionSubagents";
import { useChatViewStore } from "../src/renderer/components/chat/chatViewStore";
import { applySessionEventToAssistantTurn } from "../src/renderer/components/chat/chatTranscriptRestore";
import { listAgentRuns, listSessions } from "../src/renderer/lib/chatBridge";
import type { AgentRunSummary, SessionItem } from "../src/renderer/lib/chatBridge";
import type { SubagentResult } from "../src/renderer/components/chat/types";

const host = vi.hoisted(() => ({ handler: undefined as undefined | ((value: unknown) => void), release: vi.fn() }));
vi.mock("../src/renderer/host/hostBridge", () => ({ isNativeHostRuntime: () => true, listenHost: vi.fn(async (_name, handler) => { host.handler = handler; return host.release; }) }));
vi.mock("../src/renderer/lib/chatBridge", async original => ({
  ...await original<typeof import("../src/renderer/lib/chatBridge")>(), listSessions: vi.fn(), listAgentRuns: vi.fn(),
}));
const session = (id: string, parent: string): SessionItem => ({ id, title: id, parentSessionId: parent, runtimeJobId: `job:${id}`, sessionKind: "subagent", updatedAt: 1, messageCount: 0, activityState: "idle" });
const run = (id: string, status: string, updatedAtMs = 10): AgentRunSummary => ({ sessionId: id, agentRunId: `job:${id}`, turnId: `turn:${id}`, status, updatedAtMs, startedAtMs: 1, unread: false });
const sessions = [session("finished", "parent"), session("active", "parent"), session("cancelled", "active"), session("other", "elsewhere")];
const runs = [run("finished", "succeeded"), run("active", "running"), run("cancelled", "cancelled")];
function Tags({ id = "parent", onOpen = vi.fn() }: { id?: string; onOpen?: (id: string, title: string) => void }) {
  const { subagents } = useSessionSubagents(id);
  return <AgentSessionTags subagents={subagents} onOpenAgentSession={onOpen} />;
}
beforeEach(() => {
  vi.clearAllMocks(); useChatViewStore.getState().clear();
  vi.mocked(listSessions).mockResolvedValue(sessions);
  vi.mocked(listAgentRuns).mockResolvedValue({ agentRuns: runs });
});
afterEach(cleanup);

test("restores every descendant and its own Run state without assuming a missing state succeeded", () => {
  const restored = restoreSessionSubagents("parent", [...sessions, session("unobserved", "parent")], runs);
  expect(restored.map(entry => [entry.childSessionId, entry.status])).toEqual([
    ["finished", "done"], ["active", "running"], ["unobserved", "unknown"], ["cancelled", "cancelled"],
  ]);
  const old: SubagentResult = { ...restored[0], status: "running", updatedAtMs: 2 };
  expect(mergeSessionSubagents(restored, [old])[0].status).toBe("done");
});

test("restored tags sort running first, open the original session and survive a reopen", async () => {
  const open = vi.fn(); const user = userEvent.setup();
  const view = render(<Tags onOpen={open} />);
  const active = await screen.findByRole("button", { name: "Agent active: Running" });
  expect(screen.getAllByRole("button")[0]).toBe(active);
  expect(screen.getByRole("button", { name: "Agent cancelled: Cancelled" })).toBeTruthy();
  const mark = active.querySelector("svg")!.outerHTML;
  await user.click(active);
  expect(open).toHaveBeenCalledExactlyOnceWith("active", "active");
  view.unmount();
  expect(host.release).toHaveBeenCalledOnce();
  render(<Tags />);
  expect((await screen.findByRole("button", { name: "Agent active: Running" })).querySelector("svg")!.outerHTML).toBe(mark);
  expect(screen.getAllByRole("button")).toHaveLength(3);
});

test("a child completion notification updates a stopped parent's retained tags", async () => {
  render(<Tags />);
  const active = await screen.findByRole("button", { name: "Agent active: Running" });
  const mark = active.querySelector("svg")!.outerHTML;
  vi.mocked(listAgentRuns).mockResolvedValue({ agentRuns: runs.map(entry => entry.sessionId === "active" ? run("active", "cancelled", 20) : entry) });
  await act(async () => host.handler?.({ sessionId: "active", agentRunId: "job:active", payload: { type: "session_event", event: { type: "AgentRunInterrupted" } } }));
  const cancelled = await screen.findByRole("button", { name: "Agent active: Cancelled" });
  expect(cancelled.querySelector("svg")!.outerHTML).toBe(mark);
  expect(screen.getAllByRole("button")).toHaveLength(3);
});

test("session switching discards a late read from the previous conversation", async () => {
  let finish!: (value: SessionItem[]) => void;
  vi.mocked(listSessions).mockReturnValueOnce(new Promise(resolve => { finish = resolve; }));
  const view = render(<Tags />);
  await waitFor(() => expect(listSessions).toHaveBeenCalledOnce());
  view.rerender(<Tags id="elsewhere" />);
  await screen.findByRole("button", { name: "Agent other: Status unavailable" });
  await act(async () => finish(sessions));
  expect(screen.getAllByRole("button")).toHaveLength(1);
  expect(screen.queryByRole("button", { name: /Agent active/ })).toBeNull();
});

test("a live cancellation remains cancelled when the parent stops streaming", () => {
  const turn = applySessionEventToAssistantTurn({ id: "turn", chunks: [], finalAnswer: "", isStreaming: true }, {
    id: "cancel", version: "v1", type: "SubagentCancelled", sessionId: "parent", turnId: "turn", at: 50,
    status: "cancelled", payload: { subagentId: "child", childSessionId: "child", description: "Work" }, visibility: "user", meta: {},
  });
  expect(turn.chunks[0]).toMatchObject({ kind: "subagent", subagent: { status: "cancelled", sessionId: "parent", updatedAtMs: 50 } });
});

test("live tags stay visible when terminal hydration replaces the streamed messages", async () => {
  vi.mocked(listSessions).mockReturnValue(new Promise(() => {}));
  const subagent: SubagentResult = { id: "live", subagentId: "job:live", sessionId: "parent", childSessionId: "live-session", title: "Live work", summary: "", status: "running", updatedAtMs: 1 };
  useChatViewStore.getState().replaceMessages([{ id: "reply", role: "assistant", turn: { id: "turn", chunks: [{ id: "agent", kind: "subagent", subagent }], finalAnswer: "", isStreaming: true } }]);
  render(<Tags />);
  await screen.findByRole("button", { name: "Agent Live work: Running" });
  await act(async () => useChatViewStore.getState().replaceMessages([]));
  expect(screen.getByRole("button", { name: "Agent Live work: Running" })).toBeTruthy();
});

test("queued subagents remain active without a completion timestamp", () => {
  const turn = applySessionEventToAssistantTurn({ id: "turn", chunks: [], finalAnswer: "", isStreaming: true }, {
    id: "spawn", version: "v1", type: "SubagentSpawned", sessionId: "parent", turnId: "turn", at: 50,
    status: "queued", payload: { subagentId: "child", childSessionId: "child", description: "Work" }, visibility: "user", meta: {},
  });
  const entry = turn.chunks[0];
  expect(entry.kind === "subagent" && entry.subagent.status).toBe("queued");
  expect(entry.kind === "subagent" && entry.subagent.completedAtMs).toBeUndefined();
});
