import { memo, useLayoutEffect, type RefObject } from "react";
import { Check, Copy, Pencil } from "lucide-react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { AgentResultStream } from "./AgentResultStream";
import {
  selectChatMessageById,
  selectChatMessageIds,
  selectChatMessageRoleById,
  selectChatTurnByMessageId,
  useChatViewStore,
} from "./chatViewStore";
import { formatUserMessageTimestamp } from "./chatRuntimeModel";
import { Button } from "../ui/button";
import { Tooltip } from "../ui/tooltip";
import type { ChatMessage } from "./types";

type VirtualMessageListProps = {
  containerRef: RefObject<HTMLDivElement | null>;
  editingUserMessageId: string | null;
  editingPrompt: string;
  copiedUserMessageId: string | null;
  latestUserMessageId: string | null;
  editableUserMessageId: string | null;
  onScroll: () => void;
  onContentSizeChange: (totalSize: number) => void;
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
  const message = useChatViewStore(selectChatMessageById(messageId));
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
            >
              取消
            </button>
            <button
              type="button"
              className="user-edit-primary"
              onClick={() => onSubmitEditedUserMessage(message.id)}
              disabled={!editingPrompt.trim()}
            >
              发送
            </button>
          </div>
        </div>
      ) : (
        <div className="user-message-stack">
          <div className="message-bubble">
            <div className="message-content">
              <p>{message.text}</p>
            </div>
          </div>
          <div className={`user-message-meta-row ${isCopied ? "is-copied" : ""}`}>
            <span className="user-message-time">
              {formatUserMessageTimestamp(message.timestamp)}
            </span>
            <Tooltip content="复制">
              <Button
                type="button"
                variant="ghost"
                size="icon"
                className="user-message-icon-btn"
                onClick={() => onCopyUserMessage(message.id, message.text)}
                aria-label="复制"
              >
                {isCopied ? (
                  <Check className="user-message-action-icon" aria-hidden="true" />
                ) : (
                  <Copy className="user-message-action-icon" aria-hidden="true" />
                )}
              </Button>
            </Tooltip>
            {canEdit ? (
                  <Tooltip content="编辑">
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      className="user-message-icon-btn"
                      onClick={() => onStartEditingUserMessage(message)}
                      aria-label="编辑"
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
  onOpenAgentSession,
  onOpenWorkspacePath,
}: {
  messageId: string;
  onOpenAgentSession?: VirtualMessageListProps["onOpenAgentSession"];
  onOpenWorkspacePath?: VirtualMessageListProps["onOpenWorkspacePath"];
}) {
  const turn = useChatViewStore(selectChatTurnByMessageId(messageId));
  if (!turn) {
    return null;
  }
  return (
    <div className="message assistant-message">
      <div className="message-bubble">
        <AgentResultStream
          turn={turn}
          onOpenAgentSession={onOpenAgentSession}
          onOpenWorkspacePath={onOpenWorkspacePath}
        />
      </div>
    </div>
  );
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
  onContentSizeChange,
  onScroll,
  ...props
}: VirtualMessageListProps) {
  const messageIds = useChatViewStore(selectChatMessageIds);
  const virtualizer = useVirtualizer({
    count: messageIds.length,
    getScrollElement: () => containerRef.current,
    estimateSize: () => ESTIMATED_MESSAGE_HEIGHT_PX,
    overscan: MESSAGE_LIST_OVERSCAN,
  });
  const virtualItems = virtualizer.getVirtualItems();
  const totalSize = virtualizer.getTotalSize();

  useLayoutEffect(() => {
    onContentSizeChange(totalSize);
  }, [onContentSizeChange, totalSize]);

  return (
    <div
      className="messages-container uiRsMessagesContainer"
      ref={containerRef}
      onScroll={onScroll}
    >
      <div
        style={{
          height: `${totalSize}px`,
          position: "relative",
          width: "100%",
        }}
      >
        {virtualItems.map((item) => {
          const messageId = messageIds[item.index];
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
              <MessageRow
                messageId={messageId}
                props={props}
              />
            </div>
          );
        })}
      </div>
    </div>
  );
}
