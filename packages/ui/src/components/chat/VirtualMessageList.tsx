import { TranscriptScrollAnchor, createTranscriptScrollAnchor } from "./TranscriptScrollAnchor";
import { AssistantAnswerFooter } from "./AssistantAnswerFooter";
import { transcriptMessageGroups } from "./transcriptMessageGroups";
import { historyProcessEntries } from "./historyProcessEntries";
import { useShallow } from "zustand/react/shallow";
import { t } from "../../i18n";
import { memo, useCallback, useLayoutEffect, useMemo, useRef, type CSSProperties, type ReactNode, type RefObject } from "react";
import { Check, Copy, Pencil } from "lucide-react";
import { defaultRangeExtractor, measureElement, useVirtualizer, type Range, type Virtualizer } from "@tanstack/react-virtual";
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
import { Textarea } from "../ui/textarea";
import type { ChatMessage } from "./types";

type VirtualMessageListProps = {
  containerRef: RefObject<HTMLDivElement | null>;
  contentRef?: RefObject<HTMLDivElement | null>;
  spacerRef?: RefObject<HTMLDivElement | null>;
  onReadingIntent?: () => void;
  isFollowingLatest?: boolean;
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
const MESSAGE_LIST_BUFFER_SCREENS = 2;
const MESSAGE_LIST_MAX_BUFFER_ROWS = 60;

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
    <div data-transcript-anchor={message.id} className={`message user-message ${isEditing ? "is-editing" : ""}`} aria-busy={loading || undefined}>
      {isEditing ? (
        <div className="message-bubble user-edit-bubble">
          <Textarea
            aria-label="Edit message"
            className="user-edit-textarea"
            value={editingPrompt}
            onChange={(event) => onEditingPromptChange(event.target.value)}
            autoFocus
          />
          <div className="user-edit-actions">
            <Button variant="outline" size="sm"
              type="button"
              className="desktopUserEditCancel"
              onClick={onCancelEditingUserMessage}
            >{t("virtualMessageList.cancel")}</Button>
            <Button size="sm"
              type="button"
              className="desktopUserEditSubmit"
              onClick={() => onSubmitEditedUserMessage(message.id)}
              disabled={!editingPrompt.trim()}
            >{t("composerPromptInput.send")}</Button>
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
            {canEdit && !loading && !paged ? (
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
  completedAtMs,
  onOpenAgentSession,
  onOpenWorkspacePath,
}: {
  messageId: string;
  projectedMessage?: ChatMessage;
  isStandaloneRow?: boolean;
  completedAtMs?: number;
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
  const { message, status, loading } = useTranscriptText(storedMessage, !toolOnly && (!reasoning || expanded));
  if (!message || message.role !== "assistant") {
    return null;
  }
  return (
    <div data-transcript-anchor={message.id} className={`message assistant-message ${isStandaloneRow ? "" : "historyProcessRow"}`} aria-busy={loading || undefined}>
      <div className="message-bubble">
        {status}
        <AgentResultStream
          turn={message.turn}
          onOpenAgentSession={onOpenAgentSession}
          onOpenWorkspacePath={onOpenWorkspacePath}
        />
        {!message.turn.isStreaming && !loading && message.turn.finalAnswer.trim() ? <AssistantAnswerFooter text={message.turn.finalAnswer} completedAtMs={message.turn.completedAtMs ?? completedAtMs} /> : null}
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
  const completedAtMs = final?.role === "assistant" && final.turn.projectionRunId
    ? messages.find(message => message?.role === "assistant" && message.turn.projectionRunId === final.turn.projectionRunId && message.turn.completedAtMs !== undefined)
    : undefined;
  const row = (message: ChatMessage, standalone = false) => <AssistantMessageRow key={message.id} messageId={message.id} projectedMessage={message}
    isStandaloneRow={standalone} completedAtMs={completedAtMs?.role === "assistant" ? completedAtMs.turn.completedAtMs : undefined} onOpenAgentSession={onOpenAgentSession} onOpenWorkspacePath={onOpenWorkspacePath} />;
  return <>
    {process.length ? <div className="historyProcessRows">{process.map(message => row(message))}</div> : null}
    {hasFinal && final ? row(final, true) : null}
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
function TimelineRow({ id, item, retainedHeight, messageRole, measureElement, onMeasure, children }: {
  retainedHeight?: number;
  messageRole?: "user" | "assistant";
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
    let previousHeight = -1;
    const report = (height: number) => {
      if (height === previousHeight) return;
      previousHeight = height;
      onMeasure(id, height);
    };
    if (element.current) report(element.current.getBoundingClientRect().height);
    if (typeof ResizeObserver === "undefined" || !element.current) return;
    const observer = new ResizeObserver((entries = []) => {
      const height = entries[0]?.borderBoxSize?.[0]?.blockSize ?? element.current?.getBoundingClientRect().height;
      if (height !== undefined) report(height);
    });
    observer.observe(element.current);
    return () => observer.disconnect();
  }, [id, live, onMeasure]);
  return <div ref={setElement} className="chatTimelineRow" data-message-role={messageRole} data-index={item?.index} data-live-tail={live ? id : undefined} data-history-row={live ? undefined : id}
    style={item ? {"--chat-retained-row-height":`${retainedHeight ?? 0}px`,left:0,position:"absolute",width:"100%"} as CSSProperties : {display:"flow-root"}}>{children}</div>;
}

export function VirtualMessageList({
  containerRef,
  contentRef,
  spacerRef,
  onReadingIntent,
  isFollowingLatest = false,
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
  // A page can prepend blocks inside an existing answer. Retain that row's
  // identity rather than deriving it again from the new first block.
  const committedKeys = useRef(new Map<string, string>());
  const groupKeys = historyGroups.map(ids => ids.map(id => committedKeys.current.get(id)).find(key => key !== undefined) ?? ids[0]);
  const scrollAnchor = useMemo(() => createTranscriptScrollAnchor(() => containerRef.current), [containerRef]);
  const groupIndexes = useRef(new Map<string, number>());
  groupIndexes.current = new Map(groupKeys.map((key, index) => [key, index]));
  const pauseReading = () => { scrollAnchor.manualIntent(); onReadingIntent?.(); };
  const rowHeights = useRef(new Map<string, number>());
  const virtualizerRef = useRef<Virtualizer<HTMLDivElement, HTMLDivElement> | null>(null);
  const rangeExtractor = useCallback((range: Range) => {
    const instance = virtualizerRef.current;
    const measurements = instance?.measurementsCache;
    const height = instance?.scrollRect?.height ?? containerRef.current?.clientHeight ?? 0;
    const firstVisible = measurements?.[range.startIndex];
    const lastVisible = measurements?.[range.endIndex];
    if (!height || !firstVisible || !lastVisible) return defaultRangeExtractor(range);
    const buffer = height * MESSAGE_LIST_BUFFER_SCREENS;
    let start = Math.max(0, range.startIndex - range.overscan);
    let end = Math.min(range.count - 1, range.endIndex + range.overscan);
    while (start > 0 && range.startIndex - start < MESSAGE_LIST_MAX_BUFFER_ROWS
      && measurements[start].start > firstVisible.start - buffer) start--;
    while (end < range.count - 1 && end - range.endIndex < MESSAGE_LIST_MAX_BUFFER_ROWS
      && measurements[end].end < lastVisible.end + buffer) end++;
    const indexes = Array.from({ length: end - start + 1 }, (_, offset) => start + offset);
    const retained = scrollAnchor.rowKey;
    const index = retained ? groupIndexes.current.get(retained) : undefined;
    if (index !== undefined && !indexes.includes(index)) indexes.push(index);
    return indexes.sort((a, b) => a - b);
  }, [containerRef, scrollAnchor]);
  const previousFirst = useRef<string | undefined>(undefined);
  const first = historyGroups[0]?.[0];
  const prepending = previousFirst.current !== undefined && previousFirst.current !== first
    && historyGroups.some(ids => ids.includes(previousFirst.current!));
  const userIndex = historyGroups.findIndex((ids) => ids.includes(props.latestUserMessageId ?? ""));
  const notifySize = useCallback((instance: Virtualizer<HTMLDivElement, HTMLDivElement>, measuredSize = instance.getTotalSize()) => {
    onContentSizeChange(measuredSize + (liveKey ? rowHeights.current.get(liveKey) ?? 0 : 0), instance.measurementsCache[userIndex]?.start ?? 0, props.latestUserMessageId ?? "");
  }, [onContentSizeChange, liveKey, userIndex, props.latestUserMessageId]);
  const virtualizer = useVirtualizer<HTMLDivElement, HTMLDivElement>({
    count: historyGroups.length,
    // TanStack commits row positions and spacer height together in its observer
    // callback. React scheduling cannot paint long new rows over following text.
    directDomUpdates: true,
    // The resident live tail uses normal flow; a retained top is inert there.
    directDomUpdatesMode: "position",
    onChange: instance => {
      if (isFollowingLatest) scrollAnchor.release();
      scrollAnchor.commit(instance);
      notifySize(instance);
    },
    scrollToFn: scrollAnchor.scrollToFn,
    anchorTo: prepending ? "end" : "start",
    measureElement: (element, entry, instance) => {
      // Ref callbacks run before prepend positioning. Keep their known size;
      // ResizeObserver measures the committed DOM after that position is applied.
      if (!entry) {
        const index = instance.indexFromElement(element);
        return instance.itemSizeCache.get(instance.options.getItemKey(index)) ?? instance.options.estimateSize(index);
      }
      return measureElement(element, entry, instance);
    },
    getItemKey: (index) => groupKeys[index],
    getScrollElement: () => containerRef.current,
    estimateSize: (index) => rowHeights.current.get(historyGroups[index][0]) ?? ESTIMATED_MESSAGE_HEIGHT_PX,
    overscan: MESSAGE_LIST_OVERSCAN,
    rangeExtractor,
  });
  virtualizerRef.current = virtualizer;
  virtualizer.shouldAdjustScrollPositionOnItemSizeChange = scrollAnchor.shouldAdjustSize;
  const previousTail = useRef<string | null>(null);
  useLayoutEffect(() => {
    const completedKey = previousTail.current;
    previousTail.current = liveKey;
    if (completedKey && completedKey !== liveKey) {
      const index = historyGroups.findIndex(ids => ids[0] === completedKey);
      const height = virtualizer.elementsCache?.get(completedKey)?.getBoundingClientRect().height ?? rowHeights.current.get(completedKey);
      if (index >= 0 && height !== undefined) virtualizer.resizeItem(index, height);
    }
    previousFirst.current = first;
    committedKeys.current = new Map(historyGroups.flatMap((ids, index) => ids.map(id => [id, groupKeys[index]] as const)));
  }, [liveKey, historyGroups, virtualizer, first, groupKeys]);
  const virtualItems = virtualizer.getVirtualItems();
  const totalSize = virtualizer.getTotalSize();
  useLayoutEffect(() => {
    if (isFollowingLatest) scrollAnchor.release();
    scrollAnchor.commit(virtualizer);
  });
  const touchY = useRef<number | null>(null);
  const viewportHeight = virtualizer.scrollRect?.height;
  const measureTail = useCallback((id: string, height: number) => {
    rowHeights.current.set(id, height);
    notifySize(virtualizer);
  }, [notifySize, virtualizer]);
  useLayoutEffect(() => {
    void viewportHeight;
    notifySize(virtualizer, totalSize);
  }, [notifySize, virtualizer, totalSize, viewportHeight]);
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

  const readScroll = (direction: number, viewport: HTMLDivElement) => {
    // A downward gesture already at the real end cannot move the viewport.
    // Keep following there; every gesture that can move it cancels pending follow.
    if (direction < 0 || viewport.scrollTop + viewport.clientHeight < viewport.scrollHeight - 2) pauseReading();
  };

  return (
    <div
      className="messages-container uiRsMessagesContainer"
      style={{overflowAnchor:"none"}}
      ref={containerRef}
      onScroll={() => { scrollAnchor.observeScroll(); onScroll(); }}
      onClickCapture={(event) => {
        // Pause before the disclosure changes height, including keyboard clicks.
        const disclosure = (event.target as Element).closest("button[aria-expanded]");
        if (disclosure) { scrollAnchor.hold(disclosure); onReadingIntent?.(); }
      }}
      tabIndex={0}
      onKeyDown={(event) => {
        if ((event.target as HTMLElement).closest("textarea, input, [contenteditable=true]")) return;
        if (["ArrowUp", "PageUp", "Home"].includes(event.key) || (event.key === " " && event.shiftKey)) readScroll(-1, event.currentTarget);
        else if (["ArrowDown", "PageDown", "End", " "].includes(event.key)) readScroll(1, event.currentTarget);
      }}
      onPointerDown={(event) => {
        // Native scrollbar presses target the viewport, not its message children.
        if (event.target === event.currentTarget) pauseReading();
      }}
      onTouchStart={(event) => { touchY.current = event.touches[0]?.clientY ?? null; }}
      onTouchMove={(event) => {
        const y = event.touches[0]?.clientY ?? null;
        if (y !== null && touchY.current !== null && y !== touchY.current) readScroll(touchY.current - y, event.currentTarget);
        touchY.current = y;
      }}
      onWheel={(event) => {
        if (event.deltaY !== 0) readScroll(event.deltaY, event.currentTarget);
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
      <TranscriptScrollAnchor ids={messageIds} viewportRef={containerRef} contentRef={contentRef} onPrepend={scrollAnchor.begin}>
        <div ref={virtualizer.containerRef} aria-hidden="true" />
        {[
          ...virtualItems.flatMap(item => {
            const ids = historyGroups[item.index];
            const messageId = ids?.[0];
            const rowKey = groupKeys[item.index];
            return messageId ? [<TimelineRow key={rowKey} id={rowKey} messageRole={useChatViewStore.getState().messageById[messageId]?.role} item={item} retainedHeight={virtualizer.itemSizeCache?.get(rowKey)} measureElement={virtualizer.measureElement} onMeasure={measureTail}>
              {renderGroup(ids)}
            </TimelineRow>] : [];
          }),
          ...(liveKey && lastGroup ? [<TimelineRow key={liveKey} id={liveKey} messageRole="assistant" measureElement={virtualizer.measureElement} onMeasure={measureTail}>
            {renderGroup(lastGroup)}
          </TimelineRow>] : []),
        ]}
      </TranscriptScrollAnchor>
      <div ref={spacerRef} aria-hidden="true" style={{ height: 0, flexShrink: 0 }} />
    </div>
  );
}
