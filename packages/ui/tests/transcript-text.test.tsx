import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, test, vi } from "vitest";
import { useTranscriptText } from "../src/components/chat/useTranscriptText";
import type { ChatMessage } from "../src/components/chat/types";

const read = vi.hoisted(() => vi.fn());
vi.mock("../src/components/chat/transcriptContentRanges", () => ({ loadTranscriptContentRange: read }));
globalThis.IS_REACT_ACT_ENVIRONMENT = true;

function Text({ message, enabled = true }: { message: ChatMessage; enabled?: boolean }) {
  const resolved = useTranscriptText(message, enabled);
  return <>{resolved.status}<pre>{resolved.message?.role === "assistant" ? resolved.message.turn.finalAnswer : ""}</pre></>;
}

test("Desktop pages long referenced answers on demand and drops late results after switching", async () => {
  const message: ChatMessage = { id: "answer", role: "assistant", turn: { id: "answer", chunks: [], finalAnswer: "", isStreaming: false },
    transcriptText: { sessionId: "one", projectionGeneration: "generation-1", reference: { refId: "session-event:answer:modelMarkdown", revision: "1", byteLength: "70003" } } };
  const first = "a".repeat(65536);
  const last = "b".repeat(4464) + "中";
  read.mockImplementation(async (_identity, offset) => offset === "0"
    ? { content: first, endOffset: "65536", hasMore: true }
    : { content: last, endOffset: "70003", hasMore: false });
  let renderer: ReactTestRenderer;
  await act(async () => { renderer = create(<Text message={message} />); });
  expect(renderer!.root.findByType("pre").children.join("")).toBe(first);
  expect(read).toHaveBeenCalledTimes(1);
  await act(async () => { renderer!.root.findByProps({ "aria-label": "Next content page" }).props.onClick(); });
  expect(renderer!.root.findByType("pre").children.join("")).toBe(last);
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
