import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { beforeEach, expect, test, vi } from "vitest";
import { useTranscriptText } from "../src/components/chat/useTranscriptText";
import type { ChatMessage } from "../src/components/chat/types";
import { AgentProcessTranscript } from "../src/components/chat/AgentTranscriptSections";
import { buildTranscriptProcessViewModel } from "../src/components/chat/agentTranscriptModel";

const read = vi.hoisted(() => vi.fn());
vi.mock("../src/components/chat/transcriptContentRanges", () => ({ loadTranscriptContentRange: read }));
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
beforeEach(() => { read.mockReset(); });

function Text({ message, enabled = true }: { message: ChatMessage; enabled?: boolean }) {
  const resolved = useTranscriptText(message, enabled);
  return <>{resolved.status}<pre>{resolved.message?.role === "assistant" ? resolved.message.turn.finalAnswer : resolved.message?.text ?? ""}</pre></>;
}

function ErrorText({ message }: { message: ChatMessage }) {
  const resolved = useTranscriptText(message);
  return <>{resolved.status}{resolved.message?.role === "assistant" ? <AgentProcessTranscript
    processTranscript={buildTranscriptProcessViewModel(resolved.message.turn)} agentRunId="run" onOpenWorkspacePath={undefined} /> : null}</>;
}

test("referenced run errors retry failed reads and keep plain error presentation after loading", async () => {
  read.mockReset();
  read.mockRejectedValueOnce(new Error("disconnected"));
  read.mockResolvedValueOnce({ content: "provider_timeout", endOffset: "16", hasMore: false });
  const message: ChatMessage = { id: "run-boundary:event", role: "assistant",
    turn: { id: "run", chunks: [{ id: "error", kind: "narrative", text: "", tone: "error", runFailure: "failed" }], finalAnswer: "", isStreaming: false },
    transcriptText: { sessionId: "session", projectionGeneration: "generation", reference: { refId: "ref", revision: "1", byteLength: "16" } } };
  let renderer: ReactTestRenderer;
  await act(async () => { renderer = create(<ErrorText message={message} />); });
  expect(renderer!.root.findByProps({ role: "alert" }).children.join("")).toContain("Unable to load content.");
  await act(async () => { renderer!.root.findByType("button").props.onClick(); });
  const strings = JSON.stringify(renderer!.toJSON());
  expect(strings).toContain("The model took too long to respond. Retry the request. · provider_timeout");
  expect(strings).not.toContain("Unable to load content.");
  expect(read).toHaveBeenCalledTimes(2);
  await act(async () => { renderer!.unmount(); });
});

test("Desktop assembles long referenced answers and drops late results after switching", async () => {
  const message: ChatMessage = { id: "answer", role: "assistant", turn: { id: "answer", chunks: [], finalAnswer: "", isStreaming: false },
    transcriptText: { sessionId: "one", projectionGeneration: "generation-1", reference: { refId: "session-event:answer:modelMarkdown", revision: "1", byteLength: "70003" } } };
  const first = "a".repeat(65536);
  const last = "b".repeat(4464) + "中";
  read.mockImplementation(async (_identity, offset) => offset === "0"
    ? { content: first, endOffset: "65536", hasMore: true }
    : { content: last, endOffset: "70003", hasMore: false });
  let renderer: ReactTestRenderer;
  await act(async () => { renderer = create(<Text message={message} />); });
  expect(renderer!.root.findByType("pre").children.join("")).toBe(first + last);
  expect(renderer!.root.findAllByProps({ "aria-label": "Next content page" })).toHaveLength(0);
  expect(read).toHaveBeenCalledTimes(2);
  let finish: (value: unknown) => void = () => {};
  read.mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
  const pending = { ...message, transcriptText: { ...message.transcriptText!, sessionId: "two" } };
  await act(async () => { renderer!.update(<Text message={pending} />); });
  const current: ChatMessage = { id: "new", role: "assistant", turn: { id: "new", chunks: [], finalAnswer: "new answer", isStreaming: false } };
  await act(async () => { renderer!.update(<Text message={current} />); });
  await act(async () => { finish({ content: "stale", endOffset: "5", hasMore: false }); });
  expect(renderer!.root.findByType("pre").children.join("")).toBe("new answer");
  await act(async () => { renderer!.unmount(); });
});

test("folded referenced content does not read until opened", async () => {
  read.mockClear();
  read.mockResolvedValue({ content: "opened", endOffset: "6", hasMore: false });
  const message: ChatMessage = { id: "lazy", role: "assistant", turn: { id: "lazy", chunks: [], finalAnswer: "", isStreaming: false },
    transcriptText: { sessionId: "lazy", projectionGeneration: "g", reference: { refId: "r", revision: "1", byteLength: "6" } } };
  let renderer: ReactTestRenderer;
  await act(async () => { renderer = create(<Text message={message} enabled={false} />); });
  expect(read).not.toHaveBeenCalled();
  await act(async () => { renderer!.update(<Text message={message} />); });
  expect(read).toHaveBeenCalledTimes(1);
  expect(renderer!.root.findByType("pre").children).toEqual(["opened"]);
  await act(async () => { renderer!.unmount(); });
});


test("referenced visual answers assemble UTF-8 pages before Markdown parsing", async () => {
  const fence = String.fromCharCode(96).repeat(3);
  const source = "Before\n\n" + fence + "centaeris_visual\n{\"schema\":\"centaeris.visual.v1\",\"spec\":{\"root\":\"r\",\"elements\":{\"r\":{\"type\":\"Text\",\"props\":{\"text\":\"继续工作\",\"role\":\"body\"},\"children\":[]}}}}\n" + fence + "\nAfter";
  const split = source.indexOf("继续工作");
  const first = source.slice(0, split), last = source.slice(split);
  const firstBytes = String(new TextEncoder().encode(first).length);
  const total = String(new TextEncoder().encode(source).length);
  read.mockImplementation(async (_identity, offset) => offset === "0"
    ? { content: first, endOffset: firstBytes, hasMore: true }
    : { content: last, endOffset: total, hasMore: false });
  const message: ChatMessage = { id: "visual", role: "assistant", turn: { id: "visual", chunks: [], finalAnswer: "", isStreaming: false },
    transcriptText: { sessionId: "visual", projectionGeneration: "g", reference: { refId: "r", revision: "1", byteLength: total } } };
  let renderer: ReactTestRenderer;
  await act(async () => { renderer = create(<Text message={message} />); });
  expect(renderer!.root.findByType("pre").children.join("")).toBe(source);
  expect(read.mock.calls.map((call) => call[1])).toEqual(["0", firstBytes]);
  expect(renderer!.root.findAllByType("button")).toHaveLength(0);
  await act(async () => { renderer!.unmount(); });
});


test("oversized referenced answers stop before reading while user text remains paged", async () => {
  const message: ChatMessage = { id: "big", role: "assistant", turn: { id: "big", chunks: [], finalAnswer: "", isStreaming: false },
    transcriptText: { sessionId: "big", projectionGeneration: "g", reference: { refId: "r", revision: "1", byteLength: String(16 * 1024 * 1024 + 1) } } };
  let renderer: ReactTestRenderer;
  await act(async () => { renderer = create(<Text message={message} />); });
  expect(read).not.toHaveBeenCalled();
  expect(renderer!.root.findByProps({ role: "alert" }).children.join("")).toContain("Unable to load content.");
  const user: ChatMessage = { id: "user", role: "user", text: "", transcriptText: { ...message.transcriptText!, reference: { refId: "r", revision: "2", byteLength: "10" } } };
  read.mockResolvedValue({ content: "first", endOffset: "5", hasMore: true });
  await act(async () => { renderer!.update(<Text message={user} />); });
  expect(read).toHaveBeenCalledTimes(1);
  expect(renderer!.root.findByProps({ "aria-label": "Next content page" })).toBeDefined();
  expect(renderer!.root.findByType("pre").children.join("")).toBe("first");
  await act(async () => { renderer!.unmount(); });
});
