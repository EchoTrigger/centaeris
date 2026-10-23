import { useLayoutEffect } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, expect, test, vi } from "vitest";
import { useTranscriptText } from "../src/components/chat/useTranscriptText";
import { clearTranscriptContentRangeCache } from "../src/components/chat/transcriptContentRanges";
import { getTranscriptContentRange } from "../src/lib/chatBridge";
import type { ChatMessage } from "../src/components/chat/types";

vi.mock("../src/lib/chatBridge", () => ({ getTranscriptContentRange: vi.fn(), TRANSCRIPT_PROJECTION_VERSION_V1: "transcript.projection.v1" }));
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
afterEach(() => { clearTranscriptContentRangeCache(); vi.resetAllMocks(); });

test.each([false, true])("returning to cached referenced text never commits a placeholder (multiple pages: %s)", async (multiplePages) => {
  const content = "A long answer retained across virtual row unmounts.";
  const message: ChatMessage = { id: "answer", role: "assistant", turn: { id: "answer", chunks: [], finalAnswer: "", isStreaming: false },
    transcriptText: { sessionId: "session", projectionGeneration: "generation", reference: { refId: "ref", revision: "1", byteLength: String(content.length) } } };
  vi.mocked(getTranscriptContentRange).mockImplementation(async request => {
    const startOffset = request.offset;
    const endOffset = multiplePages && startOffset === "0" ? "20" : String(content.length);
    return { schema: "transcript.content.range.v1", sessionId: "session",
      projectionVersion: "transcript.projection.v1", projectionGeneration: "generation", refId: "ref", revision: "1",
      byteLength: String(content.length), startOffset, endOffset, content: content.slice(Number(startOffset), Number(endOffset)),
      hasMore: Number(endOffset) < content.length };
  });
  const commits: { text: string; loading: boolean }[] = [];
  function Row() {
    const resolved = useTranscriptText(message);
    const text = resolved.message?.role === "assistant" ? resolved.message.turn.finalAnswer : "";
    useLayoutEffect(() => { commits.push({ text, loading: resolved.loading }); });
    return <p>{text}</p>;
  }
  let renderer!: ReactTestRenderer;
  await act(async () => { renderer = create(<Row />); });
  expect(commits.at(-1)).toEqual({ text: content, loading: false });
  await act(async () => renderer.unmount());
  commits.length = 0;
  await act(async () => { renderer = create(<Row />); });
  try {
    expect(commits.length).toBeGreaterThan(0);
    expect(commits.every(commit => commit.text === content && !commit.loading)).toBe(true);
    expect(getTranscriptContentRange).toHaveBeenCalledTimes(multiplePages ? 2 : 1);
  } finally { await act(async () => renderer.unmount()); }
});
