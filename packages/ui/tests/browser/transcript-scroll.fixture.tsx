import { createRoot } from "react-dom/client";
import { useEffect, useState } from "react";
import "../../src/index.css";
import { VirtualMessageList } from "../../src/components/chat/VirtualMessageList";
import { useChatScrollFollow } from "../../src/components/chat/useChatScrollFollow";
import { useChatViewStore } from "../../src/components/chat/chatViewStore";
import { clearTranscriptContentRangeCache } from "../../src/components/chat/transcriptContentRanges";
import type { ChatMessage } from "../../src/components/chat/types";
import type { TranscriptContentRangeV1, TranscriptContentRangeRequestV1 } from "../../src/lib/chatBridge";

const texts = new Map<string, string>();
const mixed = new URLSearchParams(location.search).has("mixed");
let pendingReads = 0;
let holdReads = false;
const suspendedReads: (() => void)[] = [];
let releaseOlderPage = () => {};
const build = (from: number, to: number): ChatMessage[] => Array.from({ length: to - from }, (_, n) => {
  const id = String(from + n);
  const paragraphs = mixed ? ((from + n) % 8 === 0 ? 24 : 1 + (n % 2)) : 10 + ((from + n) % 5) * 5;
  const text = Array.from({ length: paragraphs }, (_, p) =>
    "Paragraph " + id + "-" + p + ": " + "Reference text for a stable reading position. ".repeat(8)).join("\n\n");
  texts.set(id, text);
  return [
    { id: "u-" + id, role: "user", text: "Question " + id },
    { id: "a-" + id, role: "assistant", turn: { id: "a-" + id, chunks: [], finalAnswer: "", isStreaming: false },
      transcriptText: { sessionId: "probe", projectionGeneration: "g", reference: { refId: id, revision: "1", byteLength: String(new TextEncoder().encode(text).length) } } },
  ] as ChatMessage[];
}).flat();
window.centaerisHost = {
  kind: "electron", listen: async () => () => {},
  invoke: async <TResult,>(command: string, payload?: Record<string, unknown>): Promise<TResult> => {
    if (command !== "transcript/content-range") throw new Error(command);
    const request = payload!.request as TranscriptContentRangeRequestV1;
    pendingReads++;
    try {
      if (holdReads) await new Promise<void>(resolve => suspendedReads.push(resolve));
      else await new Promise(resolve => setTimeout(resolve, 120));
      const response: TranscriptContentRangeV1 = { schema: "transcript.content.range.v1", sessionId: "probe",
        projectionVersion: "transcript.projection.v1", projectionGeneration: "g", refId: request.refId,
        revision: "1", byteLength: request.byteLength, startOffset: "0", endOffset: request.byteLength,
        content: texts.get(request.refId)!, hasMore: false };
      return response as TResult;
    } finally { pendingReads--; }
  },
};
useChatViewStore.getState().replaceMessages(build(10, 60));
const noop = () => {};
declare global {
  interface Window {
    scrollFixture: {
      follow: ReturnType<typeof useChatScrollFollow>;
      geometry: () => { top: number; height: number; viewport: number; following: boolean; pendingReads: number };
      loadOlder: () => void;
      releaseOlder: () => void;
      evictAndHoldReads: () => void;
      resumeReads: () => void;
    };
  }
}
function Fixture() {
  const follow = useChatScrollFollow("probe");
  const [hasOlder, setHasOlder] = useState(true);
  useEffect(() => {
    window.scrollFixture = { follow, geometry: () => {
      const element = follow.messagesContainerRef.current!;
      return { top: element.scrollTop, height: element.scrollHeight, viewport: element.clientHeight, following: follow.isFollowingLatest, pendingReads };
    }, loadOlder: () => { releaseOlderPage = () => {
      const store = useChatViewStore.getState();
      store.replaceMessages([...build(0, 10), ...store.messageIds.map(id => store.messageById[id])]);
      setHasOlder(false);
    }; }, releaseOlder: () => releaseOlderPage(),
      evictAndHoldReads: () => { clearTranscriptContentRangeCache(); holdReads = true; },
      resumeReads: () => { holdReads = false; suspendedReads.splice(0).forEach(resolve => resolve()); },
    };
  });
  return <div className="chat-container" style={{ height: "100vh" }}><VirtualMessageList
    containerRef={follow.messagesContainerRef} contentRef={follow.contentRef} spacerRef={follow.spacerRef}
    isFollowingLatest={follow.isFollowingLatest} onReadingIntent={follow.pauseFollowing} onContentSizeChange={follow.scheduleFollowLatestScroll} onScroll={follow.handleMessagesScroll}
    hasOlder={hasOlder} onLoadOlder={() => window.scrollFixture.loadOlder()} editingUserMessageId={null} editingPrompt=""
    copiedUserMessageId={null} latestUserMessageId={null} editableUserMessageId={null} onEditingPromptChange={noop}
    onCancelEditingUserMessage={noop} onSubmitEditedUserMessage={noop} onCopyUserMessage={noop} onStartEditingUserMessage={noop}
  /></div>;
}
createRoot(document.getElementById("fixture")!).render(<Fixture />);
