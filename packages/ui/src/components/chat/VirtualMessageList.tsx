import { transcriptMessageGroups } from "./transcriptMessageGroups";
import { historyProcessEntries } from "./historyProcessEntries";
import { useShallow } from "zustand/react/shallow";
import { t } from "../../i18n";
import { memo, useCallback, useLayoutEffect, useMemo, useRef, type ReactNode, type RefObject } from "react";
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
  onReadingIntent?: () => void;
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
  const { message, status, loading, paged } = useTranscriptText(storedMessage);
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
            {canEdit && !loading && !paged ? (
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
  projectedMessage,
  isStandaloneRow = true,
  onOpenAgentSession,
  onOpenWorkspacePath,
}: {
  messageId: string;
  projectedMessage?: ChatMessage;
  isStandaloneRow?: boolean;
  onOpenAgentSession?: VirtualMessageListProps["onOpenAgentSession"];
  onOpenWorkspacePath?: VirtualMessageListProps["onOpenWorkspacePath"];
}) {
  const stored = useChatViewStore(selectChatMessageById(messageId));
  const storedMessage = projectedMessage ?? stored;
  const reasoning = storedMessage?.role === "assistant" && storedMessage.turn.chunks.length === 1
    && storedMessage.turn.chunks[0].kind === "reasoning" ? storedMessage.turn.chunks[0] : undefined;
  const reasoningIdentity = reasoning ? JSON.stringify([reasoning.turnId ?? reasoning.id, reasoning.id]) : "";
  const expanded = useChatViewStore(state => Boolean(state.expandedReasoning[reasoningIdentity]));
  const toolOnly = storedMessage?.role === "assistant" && storedMessage.turn.chunks.length > 0
    && storedMessage.turn.chunks.every(chunk => chunk.kind === "task");
  const { message, status } = useTranscriptText(storedMessage, !toolOnly && (!reasoning || expanded));
  if (!message || message.role !== "assistant") {
    return null;
  }
  return (
    <div className={`message assistant-message ${isStandaloneRow ? "" : "historyProcessRow"}`}>
      <div className="message-bubble">
        {status}
        <AgentResultStream
          turn={message.turn}
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
  const entries = useMemo(() => historyProcessEntries(messages), [messages]);
  const final = entries.at(-1);
  const hasFinal = Boolean(final && isAnswer(final));
  const process = hasFinal ? entries.slice(0, -1) : entries;
  const row = (message: ChatMessage) => <AssistantMessageRow key={message.id} messageId={message.id} projectedMessage={message}
    isStandaloneRow={false} onOpenAgentSession={onOpenAgentSession} onOpenWorkspacePath={onOpenWorkspacePath} />;
  return <>
    {process.length ? <div className="historyProcessRows">{process.map(row)}</div> : null}
    {hasFinal && final ? row(final) : null}
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

// Both layouts keep the same keyed component and DOM parent, so completing a
// visible tail does not reset an open output page, selection or inner scroll.
function TimelineRow({ id, item, measureElement, onMeasure, children }: {
  id: string;
  item?: { index: number; start: number };
  measureElement: (element: HTMLDivElement | null) => void;
  onMeasure: (id: string, height: number) => void;
  children: ReactNode;
}) {
  const element = useRef<HTMLDivElement>(null);
  const live = item === undefined;
  const setElement = useCallback((node: HTMLDivElement | null) => {
    element.current = node;
    if (!live) measureElement(node);
  }, [live, measureElement]);
  useLayoutEffect(() => {
    if (!live) return;
    const measure = () => { if (element.current) onMeasure(id, element.current.getBoundingClientRect().height); };
    measure();
    if (typeof ResizeObserver === "undefined" || !element.current) return;
    const observer = new ResizeObserver(measure);
    observer.observe(element.current);
    return () => observer.disconnect();
  }, [id, live, onMeasure]);
  return <div ref={setElement} data-index={item?.index} data-live-tail={live ? id : undefined} data-history-row={live ? undefined : id}
    style={item ? {left:0,position:"absolute",top:0,transform:`translateY(${item.start}px)`,width:"100%"} : {display:"flow-root"}}>{children}</div>;
}

export function VirtualMessageList({
  containerRef,
  contentRef,
  spacerRef,
  onReadingIntent,
  hasOlder = false,
  isLoadingOlder = false,
  onLoadOlder,
  onContentSizeChange,
  onScroll,
  ...props
}: VirtualMessageListProps) {
  const messageIds = useChatViewStore(selectChatMessageIds);
  const groups = useMemo(() => transcriptMessageGroups(messageIds, (id) => useChatViewStore.getState().messageById[id]), [messageIds]);
  const lastGroup = groups.at(-1);
  const liveKey = useChatViewStore(state => {
    const message = lastGroup?.length === 1 ? state.messageById[lastGroup[0]] : undefined;
    return message?.role === "assistant" && message.turn.isStreaming ? message.id : null;
  });
  const historyGroups = liveKey ? groups.slice(0, -1) : groups;
  const rowHeights = useRef(new Map<string, number>());
  const virtualizer = useVirtualizer({
    count: historyGroups.length,
    getItemKey: (index) => historyGroups[index][0],
    getScrollElement: () => containerRef.current,
    estimateSize: (index) => rowHeights.current.get(historyGroups[index][0]) ?? ESTIMATED_MESSAGE_HEIGHT_PX,
    overscan: MESSAGE_LIST_OVERSCAN,
  });
  // A visible row grows down from its title. Only changes entirely above the
  // viewport need virtualizer compensation to preserve the reading position.
  virtualizer.shouldAdjustScrollPositionOnItemSizeChange = (item, _delta, instance) =>
    item.end <= (instance.scrollOffset ?? 0) && instance.scrollDirection !== "backward";
  const previousTail = useRef<string | null>(null);
  const previousFirst = useRef<string | undefined>(undefined);
  useLayoutEffect(() => {
    const completedKey = previousTail.current;
    previousTail.current = liveKey;
    if (completedKey && completedKey !== liveKey) {
      const index = historyGroups.findIndex(ids => ids[0] === completedKey);
      const height = virtualizer.elementsCache?.get(completedKey)?.getBoundingClientRect().height ?? rowHeights.current.get(completedKey);
      if (index >= 0 && height !== undefined) virtualizer.resizeItem(index, height);
    }
    // A prepended page shifts existing rows. Preserve the old first row's
    // screen offset; completion/appending does not trigger this correction.
    const first = historyGroups[0]?.[0];
    if (previousFirst.current && previousFirst.current !== first) {
      const index = historyGroups.findIndex(ids => ids[0] === previousFirst.current);
      const offset = index > 0 ? virtualizer.measurementsCache[index]?.start : undefined;
      if (offset !== undefined && containerRef.current) containerRef.current.scrollTop += offset;
    }
    previousFirst.current = first;
  }, [liveKey, historyGroups, virtualizer, containerRef]);
  const virtualItems = virtualizer.getVirtualItems();
  const totalSize = virtualizer.getTotalSize();
  const touchY = useRef<number | null>(null);
  const userIndex = historyGroups.findIndex((ids) => ids.includes(props.latestUserMessageId ?? ""));
  const anchorTop = virtualizer.measurementsCache[userIndex]?.start ?? 0;

  const viewportHeight = virtualizer.scrollRect?.height;
  const notifySize = useCallback(() => {
    onContentSizeChange(totalSize + (liveKey ? rowHeights.current.get(liveKey) ?? 0 : 0), anchorTop, props.latestUserMessageId ?? "");
  }, [onContentSizeChange, totalSize, liveKey, anchorTop, props.latestUserMessageId]);
  const measureTail = useCallback((id: string, height: number) => {
    rowHeights.current.set(id, height);
    notifySize();
  }, [notifySize]);
  useLayoutEffect(() => {
    void viewportHeight;
    notifySize();
  }, [notifySize, viewportHeight]);
  useLayoutEffect(() => {
    const retained = new Set(messageIds);
    for (const key of rowHeights.current.keys()) if (!retained.has(key)) rowHeights.current.delete(key);
  }, [messageIds]);
  const renderGroup = (ids: string[]) => {
    const messageId = ids[0];
    const message = useChatViewStore.getState().messageById[messageId];
    return message?.role === "assistant" && !message.turn.agentRunId
      ? <HistoryProcessGroup ids={ids} onOpenAgentSession={props.onOpenAgentSession} onOpenWorkspacePath={props.onOpenWorkspacePath} />
      : <MessageRow messageId={messageId} props={props} />;
  };

  return (
    <div
      className="messages-container uiRsMessagesContainer"
      style={{overflowAnchor:"none"}}
      ref={containerRef}
      onScroll={onScroll}
      onClickCapture={(event) => {
        // Pause before the disclosure changes height, including keyboard clicks.
        if ((event.target as Element).closest("button[aria-expanded]")) onReadingIntent?.();
      }}
      tabIndex={0}
      onKeyDown={(event) => {
        if ((event.target as HTMLElement).closest("textarea, input, [contenteditable=true]")) return;
        if (["ArrowUp", "PageUp", "Home"].includes(event.key) || (event.key === " " && event.shiftKey)) onReadingIntent?.();
      }}
      onTouchStart={(event) => { touchY.current = event.touches[0]?.clientY ?? null; }}
      onTouchMove={(event) => {
        const y = event.touches[0]?.clientY ?? null;
        if (y !== null && touchY.current !== null && y > touchY.current) onReadingIntent?.();
        touchY.current = y;
      }}
      onWheel={(event) => {
        if (event.deltaY < 0) onReadingIntent?.();
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
      <div ref={contentRef} style={{width:"100%",position:"relative",display:"flow-root"}}>
        <div aria-hidden="true" style={{height:`${totalSize}px`}} />
        {[
          ...virtualItems.flatMap(item => {
            const ids = historyGroups[item.index];
            const messageId = ids?.[0];
            return messageId ? [<TimelineRow key={messageId} id={messageId} item={item} measureElement={virtualizer.measureElement} onMeasure={measureTail}>
              {renderGroup(ids)}
            </TimelineRow>] : [];
          }),
          ...(liveKey && lastGroup ? [<TimelineRow key={liveKey} id={liveKey} measureElement={virtualizer.measureElement} onMeasure={measureTail}>
            {renderGroup(lastGroup)}
          </TimelineRow>] : []),
        ]}
      </div>
      <div ref={spacerRef} aria-hidden="true" style={{ height: 0, flexShrink: 0 }} />
    </div>
  );
}
