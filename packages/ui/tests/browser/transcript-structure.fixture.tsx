import { createRoot } from "react-dom/client";
import { useEffect, useState } from "react";
import "../../src/index.css";
import { VirtualMessageList } from "../../src/components/chat/VirtualMessageList";
import { useChatScrollFollow } from "../../src/components/chat/useChatScrollFollow";
import { useChatViewStore } from "../../src/components/chat/chatViewStore";
import { DesktopTranscriptView } from "../../src/components/chat/transcriptPaging";
import type { TranscriptBlockV1, TranscriptPageV1 } from "../../src/lib/chatBridge";

// Sanitized shape of the observed transcript: six turns, 302 blocks, five
// pages. Page cuts deliberately split an answer and consecutive tool groups.
const delayedVisual = new URLSearchParams(location.search).has("delayedVisual");
const visualElements: Record<
  string,
  { type: string; props: Record<string, string>; children: string[] }
> = {
  root: {
    type: "Card",
    props: { title: "Earlier result" },
    children: Array.from({ length: 12 }, (_, i) => "step-" + i),
  },
};
for (let i = 0; i < 12; i++)
  visualElements["step-" + i] = {
    type: "Text",
    props: {
      role: "body",
      text:
        "Step " +
        i +
        ": " +
        "A detailed result that is rendered after the visual module loads. ".repeat(8),
    },
    children: [],
  };
const visualAnswer =
  "Earlier answer.\n\n" +
  String.fromCharCode(96).repeat(3) +
  "centaeris_visual\n" +
  JSON.stringify({
    schema: "centaeris.visual.v1",
    spec: { root: "root", elements: visualElements },
  }) +
  "\n" +
  String.fromCharCode(96).repeat(3) +
  "\n\nEarlier answer footer.";
const blocks: TranscriptBlockV1[] = [];
function add(body: TranscriptBlockV1["body"]) {
  const sequence = String(blocks.length + 1);
  blocks.push({
    blockId: "block-" + sequence,
    blockRevision: "1",
    orderKey: { sourceSequence: sequence, ordinal: 0 },
    body,
  });
}
for (let turn = 0; turn < 6; turn++) {
  add({ kind: "userText", content: { inlineContent: "Question " + turn } });
  add({
    kind: "notice",
    noticeType: "model_process_summary",
    status: turn < 3 ? "running" : "completed",
    content: { inlineContent: "Inspecting project " + turn },
  });
  for (let step = 0; step < 22; step++) {
    add({
      kind: "reasoning",
      requestId: "request-" + turn + "-" + step,
      status: "completed",
      content: { inlineContent: "Reasoning " + turn + "-" + step },
    });
    add({
      kind: "tool",
      status: "completed",
      callId: "call-" + turn + "-" + step,
      toolName: "exec_command",
      summary: "Inspect source " + turn + "-" + step,
      summaryRef: null,
      outputRef: null,
    });
  }
  if (turn === 0 || turn === 3)
    add({
      kind: "notice",
      noticeType: "model_process_summary",
      status: "completed",
      content: { inlineContent: "Review complete " + turn },
    });
  add({
    kind: "notice",
    noticeType: "model_process_summary",
    status: "completed",
    content: { inlineContent: "Checked implementation " + turn },
  });
  add({
    kind: "tool",
    status: "completed",
    callId: "check-" + turn,
    toolName: "exec_command",
    summary: "Verify change " + turn,
    summaryRef: null,
    outputRef: null,
  });
  add({
    kind: "assistantText",
    status: "completed",
    content: {
      inlineContent:
        delayedVisual && turn === 4
          ? visualAnswer
          : "Answer " + turn + ". " + "Validated result. ".repeat(28),
    },
  });
  add({
    kind: "notice",
    noticeType: "run_boundary",
    status: "completed",
    content: { inlineContent: "" },
  });
}
const short = new URLSearchParams(location.search).has("short");
const cuts = delayedVisual
  ? [252, 202, 0]
  : short
    ? [270, 240, 170, 100, 0]
    : [240, 170, 100, 35, 0];
let next = 1;
let pagePending = false;
const page = (from: number, to: number, hasOlder: boolean): TranscriptPageV1 => ({
  schema: "transcript.page.v1",
  sessionId: "structure",
  projectionVersion: "transcript.projection.v1",
  projectionGeneration: "g",
  sourceHighWater: String(blocks.length),
  blocks: blocks.slice(from, to),
  olderCursor: hasOlder ? "before-" + from : null,
  hasOlder,
  resumeCursors: [],
});
const view = DesktopTranscriptView.open(page(cuts[0], blocks.length, true));
useChatViewStore.getState().replaceMessages(view.materializeMessages(false));
const noop = () => {};
function Fixture() {
  const follow = useChatScrollFollow("structure");
  const [hasOlder, setHasOlder] = useState(true);
  const [loading, setLoading] = useState(false);
  useEffect(() => {
    Object.assign(window, {
      structureFixture: {
        follow,
        loading: () => pagePending,
        loadOlder: () => {
          if (pagePending || next >= cuts.length) return;
          const apply = () => {
            const from = cuts[next],
              to = cuts[next - 1];
            view.applyOlderPage(page(from, to, next < cuts.length - 1));
            useChatViewStore.getState().replaceMessages(view.materializeMessages(false));
            next++;
            setHasOlder(view.hasOlder);
            pagePending = false;
            setLoading(false);
          };
          if (delayedVisual) {
            pagePending = true;
            setLoading(true);
            setTimeout(apply, 50);
          } else apply();
        },
        count: blocks.length,
      },
    });
  });
  return (
    <div
      className="chat-container"
      style={
        delayedVisual
          ? { height: "calc(100vh - 180px)", width: "calc(100vw - 230px)", marginLeft: 230 }
          : { height: "100vh" }
      }
    >
      <VirtualMessageList
        containerRef={follow.messagesContainerRef}
        contentRef={follow.contentRef}
        spacerRef={follow.spacerRef}
        isFollowingLatest={follow.isFollowingLatest}
        onReadingIntent={follow.pauseFollowing}
        onContentSizeChange={follow.scheduleFollowLatestScroll}
        onScroll={follow.handleMessagesScroll}
        hasOlder={hasOlder}
        isLoadingOlder={loading}
        onLoadOlder={delayedVisual ? () => window.structureFixture.loadOlder() : undefined}
        editingUserMessageId={null}
        editingPrompt=""
        copiedUserMessageId={null}
        latestUserMessageId={null}
        editableUserMessageId={null}
        onEditingPromptChange={noop}
        onCancelEditingUserMessage={noop}
        onSubmitEditedUserMessage={noop}
        onCopyUserMessage={noop}
        onStartEditingUserMessage={noop}
      />
    </div>
  );
}
createRoot(document.getElementById("fixture")!).render(<Fixture />);
