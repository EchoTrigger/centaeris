import { WorkProgress } from "./WorkProgress";
import { transcriptMessageGroups } from "./transcriptMessageGroups";
import { useShallow } from "zustand/react/shallow";
import { t } from "../../i18n";
import { memo, useLayoutEffect, useMemo, useRef, type RefObject } from "react";
import { Check, Copy, Pencil } from "lucide-react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { AgentResultStream } from "./AgentResultStream";
import { useTranscriptText } from "./useTranscriptText";
import {
  selectChatMessageById,
  selectChatMessageIds,
  selectChatMessageRoleById,
  useChatViewStore,
} from "./chatViewStore";
import { formatUserMessageTimestamp } from "./chatRuntimeModel";
import { Button } from "../ui/button";
import { Tooltip } from "../ui/tooltip";
import type { ChatMessage } from "./types";

type VirtualMessageListProps = {
  containerRef: RefObject<HTMLDivElement | null>;
  contentRef?: RefObject<HTMLDivElement | null>;
  spacerRef?: RefObject<HTMLDivElement | null>;
  onUpwardIntent?: () => void;
  editingUserMessageId: string | null;
  editingPrompt: string;
  copiedUserMessageId: string | null;
  latestUserMessageId: string | null;
  editableUserMessageId: string | null;
  hasOlder?: boolean;
  isLoadingOlder?: boolean;
  onLoadOlder?: () => void;
  onScroll: () => void;
  onContentSizeChange: (totalSize: number, anchorTop: number, userId: string) => void;
  onEditingPromptChange: (value: string) => void;
  onCancelEditingUserMessage: () => void;
  onSubmitEditedUserMessage: (messageId: string) => void;
  onCopyUserMessage: (messageId: string, text: string) => void;
  onStartEditingUserMessage: (message: ChatMessage & { role: "user" }) => void;
  onOpenWorkspacePath?: (
    path: string,
    options?: { startLine?: number; endLine?: number; taskId?: string },
  ) => void;
  onOpenAgentSession?: (sessionId: string, title: string) => void;
};

const ESTIMATED_MESSAGE_HEIGHT_PX = 220;
const MESSAGE_LIST_OVERSCAN = 6;

const UserMessageRow = memo(function UserMessageRow({
  messageId,
  isEditing,
  editingPrompt,
  isCopied,
  canEdit,
  onEditingPromptChange,
  onCancelEditingUserMessage,
  onSubmitEditedUserMessage,
  onCopyUserMessage,
  onStartEditingUserMessage,
}: {
  messageId: string;
  isEditing: boolean;
  editingPrompt: string;
  isCopied: boolean;
  canEdit: boolean;
  onEditingPromptChange: (value: string) => void;
  onCancelEditingUserMessage: () => void;
  onSubmitEditedUserMessage: (messageId: string) => void;
  onCopyUserMessage: (messageId: string, text: string) => void;
  onStartEditingUserMessage: (message: ChatMessage & { role: "user" }) => void;
}) {
  const storedMessage = useChatViewStore(selectChatMessageById(messageId));
  const { message, status, loading } = useTranscriptText(storedMessage);
  if (!message || message.role !== "user") {
    return null;
  }
  return (
    <div className={`message user-message ${isEditing ? "is-editing" : ""}`}>
      {isEditing ? (
        <div className="message-bubble user-edit-bubble">
          <textarea
            className="user-edit-textarea"
            value={editingPrompt}
            onChange={(event) => onEditingPromptChange(event.target.value)}
            autoFocus
          />
          <div className="user-edit-actions">
            <button
              type="button"
              className="user-edit-secondary"
              onClick={onCancelEditingUserMessage}
            >{t("virtualMessageList.cancel")}</button>
            <button
              type="button"
              className="user-edit-primary"
              onClick={() => onSubmitEditedUserMessage(message.id)}
              disabled={!editingPrompt.trim()}
            >{t("composerPromptInput.send")}</button>
          </div>
        </div>
      ) : (
        <div className="user-message-stack">
          <div className="message-bubble">
            <div className="message-content">
              {status}<p>{message.text}</p>
            </div>
          </div>
          <div className={`user-message-meta-row ${isCopied ? "is-copied" : ""}`}>
            <span className="user-message-time">
              {formatUserMessageTimestamp(message.timestamp)}
            </span>
            <Tooltip content={t("virtualMessageList.copy")}>
              <Button
                type="button"
                variant="ghost"
                size="icon"
                className="user-message-icon-btn"
                onClick={() => onCopyUserMessage(message.id, message.text)}
                disabled={loading}
                aria-label={t("virtualMessageList.copy")}
              >
                {isCopied ? (
                  <Check className="user-message-action-icon" aria-hidden="true" />
                ) : (
                  <Copy className="user-message-action-icon" aria-hidden="true" />
                )}
              </Button>
            </Tooltip>
            {canEdit && !loading ? (
                  <Tooltip content={t("chatArea.edit")}>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      className="user-message-icon-btn"
                      onClick={() => onStartEditingUserMessage(message)}
                      aria-label={t("chatArea.edit")}
                    >
                      <Pencil
                        className="user-message-action-icon"
                        aria-hidden="true"
                      />
                    </Button>
                  </Tooltip>
            ) : null}
          </div>
        </div>
      )}
    </div>
  );
});

const AssistantMessageRow = memo(function AssistantMessageRow({
  messageId,
  showWorkProgress = true,
  onOpenAgentSession,
  onOpenWorkspacePath,
}: {
  messageId: string;
  showWorkProgress?: boolean;
  onOpenAgentSession?: VirtualMessageListProps["onOpenAgentSession"];
  onOpenWorkspacePath?: VirtualMessageListProps["onOpenWorkspacePath"];
}) {
  const storedMessage = useChatViewStore(selectChatMessageById(messageId));
  const { message, status } = useTranscriptText(storedMessage);
  if (!message || message.role !== "assistant") {
    return null;
  }
  return (
    <div className="message assistant-message">
      <div className="message-bubble">
        {status}
        <AgentResultStream
          turn={message.turn}
          showWorkProgress={showWorkProgress}
          onOpenAgentSession={onOpenAgentSession}
          onOpenWorkspacePath={onOpenWorkspacePath}
        />
      </div>
    </div>
  );
});

const HistoryProcessGroup = memo(function HistoryProcessGroup({ ids, onOpenAgentSession, onOpenWorkspacePath }: {
  ids: string[];
  onOpenAgentSession?: VirtualMessageListProps["onOpenAgentSession"];
  onOpenWorkspacePath?: VirtualMessageListProps["onOpenWorkspacePath"];
}) {
  const messages = useChatViewStore(useShallow((state) => ids.map((id) => state.messageById[id])));
  const isAnswer = (message: ChatMessage) => message.role === "assistant"
    && (Boolean(message.turn.finalAnswer) || Boolean(message.transcriptText && !message.turn.chunks.length));
  const row = (message: ChatMessage) => <AssistantMessageRow key={message.id} messageId={message.id}
    showWorkProgress={false} onOpenAgentSession={onOpenAgentSession} onOpenWorkspacePath={onOpenWorkspacePath} />;
  return <>
    <WorkProgress running={false} finalStarted={messages.some(isAnswer)}>
      {messages.filter((message) => !isAnswer(message)).map(row)}
    </WorkProgress>
    {messages.filter(isAnswer).map(row)}
  </>;
});

const MessageRow = memo(function MessageRow({
  messageId,
  props,
}: {
  messageId: string;
  props: Omit<
    VirtualMessageListProps,
    "containerRef" | "onScroll" | "onContentSizeChange"
  >;
}) {
  const role = useChatViewStore(selectChatMessageRoleById(messageId));
  if (!role) {
    return null;
  }
  if (role === "user") {
    return (
      <UserMessageRow
        messageId={messageId}
        isEditing={props.editingUserMessageId === messageId}
        editingPrompt={props.editingPrompt}
        isCopied={props.copiedUserMessageId === messageId}
        canEdit={
          props.latestUserMessageId === messageId &&
          props.editableUserMessageId === messageId
        }
        onEditingPromptChange={props.onEditingPromptChange}
        onCancelEditingUserMessage={props.onCancelEditingUserMessage}
        onSubmitEditedUserMessage={props.onSubmitEditedUserMessage}
        onCopyUserMessage={props.onCopyUserMessage}
        onStartEditingUserMessage={props.onStartEditingUserMessage}
      />
    );
  }
  return (
    <AssistantMessageRow
      messageId={messageId}
      onOpenAgentSession={props.onOpenAgentSession}
      onOpenWorkspacePath={props.onOpenWorkspacePath}
    />
  );
});

export function VirtualMessageList({
  containerRef,
  contentRef,
  spacerRef,
  onUpwardIntent,
  hasOlder = false,
  isLoadingOlder = false,
  onLoadOlder,
  onContentSizeChange,
  onScroll,
  ...props
}: VirtualMessageListProps) {
  const messageIds = useChatViewStore(selectChatMessageIds);
  const groups = useMemo(() => transcriptMessageGroups(messageIds, (id) => useChatViewStore.getState().messageById[id]), [messageIds]);
  const virtualizer = useVirtualizer({
    count: groups.length,
    getScrollElement: () => containerRef.current,
    estimateSize: () => ESTIMATED_MESSAGE_HEIGHT_PX,
    overscan: MESSAGE_LIST_OVERSCAN,
  });
  const virtualItems = virtualizer.getVirtualItems();
  const totalSize = virtualizer.getTotalSize();
  const touchY = useRef<number | null>(null);
  const userIndex = groups.findIndex((ids) => ids.includes(props.latestUserMessageId ?? ""));
  const anchorTop = virtualizer.measurementsCache[userIndex]?.start ?? 0;

  const viewportHeight = virtualizer.scrollRect?.height;
  useLayoutEffect(() => {
    void viewportHeight;
    onContentSizeChange(totalSize, anchorTop, props.latestUserMessageId ?? "");
  }, [onContentSizeChange, totalSize, anchorTop, props.latestUserMessageId, viewportHeight]);

  return (
    <div
      className="messages-container uiRsMessagesContainer"
      ref={containerRef}
      onScroll={onScroll}
      tabIndex={0}
      onKeyDown={(event) => {
        if ((event.target as HTMLElement).closest("textarea, input, [contenteditable=true]")) return;
        if (["ArrowUp", "PageUp", "Home"].includes(event.key) || (event.key === " " && event.shiftKey)) onUpwardIntent?.();
      }}
      onTouchStart={(event) => { touchY.current = event.touches[0]?.clientY ?? null; }}
      onTouchMove={(event) => {
        const y = event.touches[0]?.clientY ?? null;
        if (y !== null && touchY.current !== null && y > touchY.current) onUpwardIntent?.();
        touchY.current = y;
      }}
      onWheel={(event) => {
        if (event.deltaY < 0) onUpwardIntent?.();
        if (
          event.deltaY < 0 &&
          event.currentTarget.scrollTop <= 0 &&
          hasOlder &&
          !isLoadingOlder
        ) {
          onLoadOlder?.();
        }
      }}
    >
      <div
        ref={contentRef}
        style={{
          height: `${totalSize}px`,
          position: "relative",
          width: "100%",
        }}
      >
        {virtualItems.map((item) => {
          const ids = groups[item.index];
          const messageId = ids?.[0];
          if (!messageId) {
            return null;
          }
          return (
            <div
              key={messageId}
              data-index={item.index}
              ref={virtualizer.measureElement}
              style={{
                left: 0,
                position: "absolute",
                top: 0,
                transform: `translateY(${item.start}px)`,
                width: "100%",
              }}
            >
              {(() => {
                const message = useChatViewStore.getState().messageById[messageId];
                return message.role === "assistant" && !message.turn.agentRunId
                  ? <HistoryProcessGroup ids={ids} onOpenAgentSession={props.onOpenAgentSession} onOpenWorkspacePath={props.onOpenWorkspacePath} />
                  : <MessageRow messageId={messageId} props={props} />;
              })()}
            </div>
          );
        })}
      </div>
      <div ref={spacerRef} aria-hidden="true" style={{ height: 0, flexShrink: 0 }} />
    </div>
  );
}
