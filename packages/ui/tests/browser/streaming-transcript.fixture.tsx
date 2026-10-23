import { createRoot } from "react-dom/client";
import { useEffect, useState } from "react";
import "../../src/index.css";
import { VirtualMessageList } from "../../src/components/chat/VirtualMessageList";
import { useChatScrollFollow } from "../../src/components/chat/useChatScrollFollow";
import { useChatViewStore } from "../../src/components/chat/chatViewStore";
import { applySessionEventToAssistantTurn } from "../../src/components/chat/chatTranscriptRestore";
import type { AssistantExecutionTurn } from "../../src/components/chat/types";
import type { SessionEvent } from "../../src/lib/chatBridge";

let turn: AssistantExecutionTurn = {
  id: "stream-check",
  agentRunId: "run-check",
  chunks: [],
  finalAnswer: "",
  isStreaming: true,
};
let sequence = 0;
useChatViewStore.getState().replaceMessages([
  { id: "earlier-question", role: "user", text: "Earlier question" },
  {
    id: "earlier-answer",
    role: "assistant",
    turn: {
      id: "earlier",
      chunks: [],
      isStreaming: false,
      finalAnswer: Array.from(
        { length: 24 },
        (_, i) => "Earlier paragraph " + i + ". " + "Stable reading text. ".repeat(12),
      ).join("\n\n"),
    },
  },
  { id: "question", role: "user", text: "Streaming verification" },
  { id: "reply", role: "assistant", turn },
]);
const sentScenario = new URLSearchParams(location.search).has("sent");
if (sentScenario) {
  const store = useChatViewStore.getState();
  store.replaceMessages([
    { id: "hydrating-question", role: "user", text: "Older history question" },
    { id: "hydrating-answer", role: "assistant", turn: { id: "hydrating-answer", chunks: [], isStreaming: false, finalAnswer: "Older history still being measured." } },
    ...store.messageIds.filter(id => id.startsWith("earlier-")).map(id => store.messageById[id]),
  ]);
}
const noop = () => {};
function Fixture() {
  const follow = useChatScrollFollow("stage-probe");
  const [latestUserMessageId, setLatestUserMessageId] = useState(sentScenario ? "earlier-question" : "question");
  const [composerHeight, setComposerHeight] = useState(180);
  useEffect(() => {
    Object.assign(window, {
      stageFixture: {
        follow,
        settleComposer: () => setComposerHeight(100),
        send() {
          follow.resumeFollowingLatest();
          const store = useChatViewStore.getState();
          setLatestUserMessageId("question");
          store.replaceMessages([...store.messageIds.map(id => store.messageById[id]),
            { id: "question", role: "user", text: "Streaming verification" },
            { id: "reply", role: "assistant", turn }]);
        },
        growEarlier() {
          const store = useChatViewStore.getState();
          store.replaceMessages(store.messageIds.map(id => {
            const message = store.messageById[id];
            return id === "hydrating-answer" && message.role === "assistant"
              ? { ...message, turn: { ...message.turn, finalAnswer: message.turn.finalAnswer +
                "\n\n" + Array.from({ length: 12 }, () => "Deferred history paragraph. " + "Additional source detail. ".repeat(12)).join("\n\n") } }
              : message;
          }));
        },
        push(type: string, payload: Record<string, unknown>, extra: Partial<SessionEvent> = {}) {
          turn = applySessionEventToAssistantTurn(turn, {
            id: "stage-event-" + ++sequence,
            type,
            sessionId: "stage-probe",
            turnId: "model-turn",
            taskId: "stage-run",
            visibility: "user",
            status: "running",
            payload,
            ...extra,
          });
          useChatViewStore
            .getState()
            .updateAssistantMessages([{ id: "reply", role: "assistant", turn }]);
        },
      },
    });
  });
  return (
    <div
      className="chat-container"
      style={{ height: `calc(100vh - ${composerHeight}px)`, width: "calc(100vw - 230px)", marginLeft: 230 }}
    >
      <VirtualMessageList
        containerRef={follow.messagesContainerRef}
        contentRef={follow.contentRef}
        spacerRef={follow.spacerRef}
        isFollowingLatest={follow.isFollowingLatest}
        onReadingIntent={follow.pauseFollowing}
        onContentSizeChange={follow.scheduleFollowLatestScroll}
        onScroll={follow.handleMessagesScroll}
        editingUserMessageId={null}
        editingPrompt=""
        copiedUserMessageId={null}
        latestUserMessageId={latestUserMessageId}
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
