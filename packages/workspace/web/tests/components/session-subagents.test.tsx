// @vitest-environment jsdom
import { act, cleanup, render, renderHook, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { configureApi } from "../../src/api";
import { TranscriptToolGroupCard } from "../../src/chat/TranscriptBlockContent";
import { SessionSubagentDialog } from "../../src/chat/SessionSubagentPresentation";
import { parseSessionSubagent, parseSessionSubagents, type SessionSubagent } from "../../src/chat/sessionSubagents";
import { useSessionSubagents } from "../../src/chat/useSessionSubagents";
import { createTranscriptViewStore } from "../../src/chat/transcriptViewStore";
import { TranscriptToolOperationRegistry } from "../../src/chat/transcriptToolOperations";

vi.mock("../../src/i18n", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("../../src/chat/MarkdownContent", () => ({ MarkdownContent: ({ text }: { text: string }) => <div>{text}</div> }));
vi.mock("../../src/chat/AnimatedDisclosure", () => ({ AnimatedDisclosure: ({ expanded, children }: { expanded: boolean; children: React.ReactNode }) => expanded ? children : null }));

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

const child = (id: string, status: SessionSubagent["status"] = "succeeded"): SessionSubagent => ({
  runtimeJobId: `subagent.run:${id}`, callId: `call:${id}`, childSessionId: `child:${id}`,
  parentAgentRunId: "parent-run", parentTurnId: "parent-turn", description: `Inspect ${id}`, prompt: `Task ${id}`,
  status, createdAtMs: 100, updatedAtMs: 200,
});
const response = (agents: readonly SessionSubagent[], sessionId = "session") => ({ schema: "workspace.session.subagents.v1", sessionId, agents });
const outputRef = (agent: SessionSubagent) => ({ schema: "task_output_ref_v1", kind: "agent", runtime_job_id: agent.runtimeJobId, child_session_id: agent.childSessionId, result_ref: `subagent-result:${agent.childSessionId}` });

function fixture(tools: { id: string; toolName: string; status?: string; normalizedInput?: Record<string, unknown>; resultState?: string }[]) {
  const store = createTranscriptViewStore();
  store.openTail({ schema: "transcript.page.v1", sessionId: "session", projectionVersion: "transcript.projection.v1", projectionGeneration: "generation",
    sourceHighWater: String(tools.length), olderCursor: null, hasOlder: false, resumeCursors: [{ streamId: "workspace-transcript.v1", cursor: String(tools.length) }],
    blocks: tools.map((tool, index) => ({ blockId: tool.id, blockRevision: "1", orderKey: { sourceSequence: String(index + 1), ordinal: 0 }, body: {
      kind: "tool", callId: tool.id, toolName: tool.toolName, status: tool.status ?? "completed", summary: tool.id, summaryRef: null, outputRef: null,
    } })),
  });
  const registry = new TranscriptToolOperationRegistry();
  vi.spyOn(registry, "getSnapshot").mockReturnValue({ version: 1, records: new Map(tools.map(tool => [tool.id, {
    callId: tool.id, call: { toolName: tool.toolName, normalizedInput: tool.normalizedInput ?? {} },
    result: { resultState: tool.resultState ?? "successWithOutput" }, operations: [], status: tool.status === "failed" ? "failed" as const : "completed" as const,
  }])) });
  return { store, toolOperations: registry };
}

test("durable children stay at their creator group, with running children first inside that group", async () => {
  const done = child("done");
  const running = child("running", "running");
  const later = child("later", "cancelled");
  const props = fixture([done, running, later].map(agent => ({ id: agent.callId, toolName: "agent" })));
  const open = vi.fn();
  render(<><section aria-label="Original group"><TranscriptToolGroupCard {...props} blockIds={[done.callId, running.callId]} subagents={[done, running, later]} onOpenSubagent={open} /></section>
    <section aria-label="Later group"><TranscriptToolGroupCard {...props} blockIds={[later.callId]} subagents={[done, running, later]} onOpenSubagent={open} /></section></>);
  const original = within(screen.getByRole("region", { name: "Original group" }));
  expect(original.getAllByRole("button").map(button => button.getAttribute("aria-label"))).toEqual([
    "Agent Inspect running: sessionSubagent.status.running", "Agent Inspect done: sessionSubagent.status.succeeded",
  ]);
  expect(original.queryByRole("button", { name: /Inspect later/ })).toBeNull();
  const last = within(screen.getByRole("region", { name: "Later group" }));
  expect(last.getAllByRole("button")).toHaveLength(1);
  await userEvent.click(last.getByRole("button", { name: /Inspect later/ }));
  expect(open).toHaveBeenCalledWith(later);
});

test("only a successful canonical wait for the exact known child is folded into its visible Agent", () => {
  const known = child("known");
  const ref = outputRef(known);
  const tools = [
    { id: known.callId, toolName: "agent" },
    { id: "matched", toolName: "task_output", normalizedInput: { output_ref: ref } },
    { id: "wrong-job", toolName: "task_output", normalizedInput: { output_ref: { ...ref, runtime_job_id: "different" } } },
    { id: "wrong-schema", toolName: "task_output", normalizedInput: { output_ref: { ...ref, schema: "unknown" } } },
    { id: "failed-wait", toolName: "task_output", status: "failed", resultState: "failed", normalizedInput: { output_ref: ref } },
  ];
  const props = fixture(tools);
  render(<TranscriptToolGroupCard {...props} blockIds={tools.map(tool => tool.id)} subagents={[known]} onOpenSubagent={() => {}} />);
  expect(screen.getByRole("button", { name: /Agent Inspect known/ })).toBeTruthy();
  expect(screen.queryByText(/completed matched/)).toBeNull();
  expect(screen.getByText(/wrong-job/)).toBeTruthy();
  expect(screen.getByText(/wrong-schema/)).toBeTruthy();
  expect(screen.getByText(/failed-wait/)).toBeTruthy();
  expect(screen.getByText("transcriptTool.status.failed")).toBeTruthy();
});

test("a wait remains visible when its creator is outside the loaded transcript", () => {
  const known = child("older");
  const props = fixture([{ id: "wait", toolName: "task_output", normalizedInput: { output_ref: outputRef(known) } }]);
  render(<TranscriptToolGroupCard {...props} blockIds={["wait"]} subagents={[known]} onOpenSubagent={() => {}} />);
  expect(screen.getByText(/completed wait/)).toBeTruthy();
});

test("opening a child shows its task, reasoning, all tool rounds and final answer", async () => {
  configureApi({ apiBaseUrl: "https://example.test" });
  const agent = child("detail");
  const message = (messageId: string, role: string, content: string, more = {}) => ({ messageId, role, content, createdAtMs: 100,
    reasoningContent: null, toolCalls: [], toolCallId: null, toolName: null, resultState: null, ...more });
  const fetcher = vi.fn().mockResolvedValue(new Response(JSON.stringify({ schema: "workspace.session.subagent.v1", sessionId: "session", agent, messages: [
    message("packet", "user", "internal WorkPacket envelope"),
    message("first", "assistant", "Checking the file", { reasoningContent: "First reasoning", toolCalls: [{ id: "write", name: "write", argsJson: '{"path":"demo.txt"}' }] }),
    message("written", "tool", "File written", { toolCallId: "write", toolName: "write", resultState: "successWithOutput" }),
    message("second", "assistant", "Verifying", { toolCalls: [{ id: "read", name: "read", argsJson: '{"path":"demo.txt"}' }] }),
    message("read-back", "tool", "Verified contents", { toolCallId: "read", toolName: "read", resultState: "successWithOutput" }),
    message("answer", "assistant", "All done"),
  ] })));
  vi.stubGlobal("fetch", fetcher);
  render(<SessionSubagentDialog sessionId="session" agent={agent} revision="0" onClose={() => {}} />);
  const dialog = screen.getByRole("dialog", { name: agent.description });
  await waitFor(() => expect(within(dialog).getByText("All done")).toBeTruthy());
  expect(fetcher.mock.calls[0][0]).toBe("https://example.test/api/sessions/session/subagents/subagent.run%3Adetail");
  expect(within(dialog).getByText("Task detail")).toBeTruthy();
  expect(within(dialog).getAllByText("transcriptTool.status.completed")).toHaveLength(2);
  await userEvent.click(within(dialog).getByText("sessionSubagent.taskContext"));
  expect(within(dialog).getByText("internal WorkPacket envelope")).toBeTruthy();
  await userEvent.click(within(dialog).getByText("reasoningBlock.thoughts"));
  expect(within(dialog).getByText("First reasoning")).toBeTruthy();
  await userEvent.click(within(dialog).getByText("write"));
  expect(within(dialog).getByText("File written")).toBeTruthy();
  await userEvent.click(within(dialog).getByText("read"));
  expect(within(dialog).getByText("Verified contents")).toBeTruthy();
});

test("a committed child revision refreshes open tools and text while its Job stays running", async () => {
  configureApi({ apiBaseUrl: "https://example.test" });
  const agent = child("progress", "running");
  const assistant = (content: string, toolCalls: { id: string; name: string; argsJson: string }[] = []) => ({
    messageId: "progress-message", role: "assistant", content, createdAtMs: 100,
    reasoningContent: null, toolCalls, toolCallId: null, toolName: null, resultState: null,
  });
  const detail = (messages: unknown[]) => new Response(JSON.stringify({ schema: "workspace.session.subagent.v1", sessionId: "session", agent, messages }));
  const fetcher = vi.fn()
    .mockResolvedValueOnce(detail([assistant("First child step")]))
    .mockResolvedValueOnce(detail([assistant("Reading back the created file", [{ id: "verify", name: "read", argsJson: '{"path":"demo.txt"}' }]) ]));
  vi.stubGlobal("fetch", fetcher);
  const onClose = () => {};
  const view = render(<SessionSubagentDialog sessionId="session" agent={agent} revision="1" onClose={onClose} />);
  await waitFor(() => expect(screen.getByText("First child step")).toBeTruthy());
  view.rerender(<SessionSubagentDialog sessionId="session" agent={agent} revision="2" onClose={onClose} />);
  await waitFor(() => expect(screen.getByText("Reading back the created file")).toBeTruthy());
  expect(screen.getByText("read")).toBeTruthy();
  expect(screen.queryByText("First child step")).toBeNull();
  expect(screen.getByText("sessionSubagent.status.running")).toBeTruthy();
  expect(fetcher).toHaveBeenCalledTimes(2);
});

test("child list reads coalesce parent changes and discard results from a departed Session", async () => {
  configureApi({ apiBaseUrl: "https://example.test" });
  const resolvers: ((value: Response) => void)[] = [];
  const fetcher = vi.fn().mockImplementation(() => new Promise<Response>(resolve => resolvers.push(resolve)));
  vi.stubGlobal("fetch", fetcher);
  const view = renderHook(({ sessionId, revision }) => useSessionSubagents(sessionId, revision, true), { initialProps: { sessionId: "one", revision: "1" } });
  await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1));
  view.rerender({ sessionId: "one", revision: "2" });
  view.rerender({ sessionId: "one", revision: "3" });
  expect(fetcher).toHaveBeenCalledTimes(1);
  await act(async () => resolvers[0](new Response(JSON.stringify(response([child("one")], "one")))));
  await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(2));
  view.rerender({ sessionId: "two", revision: "3" });
  await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(3));
  await act(async () => resolvers[2](new Response(JSON.stringify(response([child("two")], "two")))));
  await waitFor(() => expect(view.result.current.agents[0]?.description).toBe("Inspect two"));
  await act(async () => resolvers[1](new Response(JSON.stringify(response([child("stale")], "one")))));
  expect(view.result.current.agents[0]?.description).toBe("Inspect two");
  await act(async () => window.dispatchEvent(new Event("focus")));
  await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(4));
});

test("child read contracts reject unknown schemas, fields and mismatched parents or jobs", () => {
  const agent = child("contract");
  expect(parseSessionSubagents(response([agent]), "session")).toEqual([agent]);
  expect(() => parseSessionSubagents({ ...response([agent]), schema: "old" }, "session")).toThrow();
  expect(() => parseSessionSubagents(response([{ ...agent, child_session_id: "alias" } as SessionSubagent]), "session")).toThrow();
  expect(() => parseSessionSubagents(response([agent]), "other-session")).toThrow();
  expect(() => parseSessionSubagent({ schema: "workspace.session.subagent.v1", sessionId: "session", agent, messages: [] }, "session", "other-job")).toThrow();
});
