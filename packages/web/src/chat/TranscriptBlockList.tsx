import { useTranscriptCitations } from "./useTranscriptCitations";
import type { CitationSummary } from "./citationSnapshot";
import { createMessageScroll } from "./messageScroll";
import {
  memo,
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type ReactNode,
  type Ref,
} from "react";
import { useTranscriptTurnMetadata } from "./useTranscriptTurnMetadata";
import { WorkProgress } from "./WorkProgress";
import { TranscriptActivity, type TranscriptActivityState } from "./TranscriptActivity";
import { groupTranscriptTurns, hasCommittedPendingMessage, shouldLoadEarlier, shouldReleaseLoadedHistory, type PendingTranscriptMessage } from "./transcriptTurns";
import { ChevronDown } from "lucide-react";
import { useTranslation } from "../i18n";
import { TranscriptBlockRow, TranscriptFailureNotice, TranscriptLiveTail, TranscriptToolGroupCard } from "./TranscriptBlockContent";
import type { TranscriptToolOperationRegistry } from "./transcriptToolOperations";
import type { TranscriptViewStore } from "./transcriptViewStore";


type Props = Readonly<{
  ref?: Ref<{ preserveReadingPosition(): void }>;
  store: TranscriptViewStore;
  toolOperations?: TranscriptToolOperationRegistry;
  sessionId: string | null;
  loadingHistory: boolean;
  loadingOlderHistory: boolean;
  onLoadOlderHistory(): Promise<void>;
  olderHistoryError?: boolean;
  emptyState?: ReactNode;
  pendingUserMessage?: PendingTranscriptMessage | null;
  submissionRejected?: boolean;
  historyMemoryPressure?: boolean;
  onReleaseLoadedHistory?(): void;
  onReadingLatestChange?(sessionId: string | null, readingLatest: boolean): void;
  onShowCitation?(citation: CitationSummary, origin: { elementId: string }): void;
  onShowArtifact?(artifact: import("./useTranscriptTurnMetadata").PublishedArtifact, origin: { elementId: string }): void;
  running?: boolean;
  activity?: TranscriptActivityState;
  startedAtMs?: number;
  completedAtMs?: number;
}>;

function useTranscriptList(store: TranscriptViewStore) {
  return useSyncExternalStore(store.subscribeList, store.getListSnapshot, store.getListSnapshot);
}

function useTranscriptLive(store: TranscriptViewStore) {
  return useSyncExternalStore(store.subscribeLive, store.getLiveSnapshot, store.getLiveSnapshot);
}

function measuredContentEnd(element: HTMLElement, content: HTMLElement) {
  return content.getBoundingClientRect().bottom - element.getBoundingClientRect().top
    + element.scrollTop + parseFloat(getComputedStyle(element).paddingBottom || "0");
}

function innerCanScroll(target: EventTarget | null, outer: HTMLElement | null, direction: number) {
  for (let node = target instanceof Element ? target : null; node && node !== outer; node = node.parentElement) {
    if (!(node instanceof HTMLElement) || !["auto", "scroll"].includes(getComputedStyle(node).overflowY)) continue;
    const maximum = node.scrollHeight - node.clientHeight;
    if (maximum > 0 && (direction < 0 ? node.scrollTop > 0 : node.scrollTop < maximum)) return true;
  }
  return false;
}

type TranscriptListEntry =
  | Readonly<{ kind: "block"; blockId: string }>
  | Readonly<{ kind: "tool"; blockIds: string[] }>;

// Consecutive tool blocks share one card, mirroring how the desktop host
// groups consecutive tool tasks.
function groupTranscriptBlocks(
  store: TranscriptViewStore,
  blockIds: readonly string[],
): TranscriptListEntry[] {
  const entries: TranscriptListEntry[] = [];
  let pendingTools: string[] = [];
  const flushTools = () => {
    if (pendingTools.length > 0) {
      entries.push({ kind: "tool", blockIds: pendingTools });
      pendingTools = [];
    }
  };
  for (const blockId of blockIds) {
    const block = store.getBlockSnapshot(blockId);
    const body = block?.body as { kind?: unknown } | undefined;
    if (body && body.kind === "tool") {
      pendingTools.push(blockId);
      continue;
    }
    flushTools();
    entries.push({ kind: "block", blockId });
  }
  flushTools();
  return entries;
}

export const TranscriptBlockList = memo(function TranscriptBlockList({
  ref,
  store,
  toolOperations,
  sessionId,
  loadingHistory,
  loadingOlderHistory,
  onLoadOlderHistory,
  olderHistoryError = false,
  emptyState,
  pendingUserMessage,
  submissionRejected = false,
  historyMemoryPressure = false,
  onReleaseLoadedHistory,
  onReadingLatestChange,
  onShowArtifact,
  onShowCitation,
  running = false,
  activity,
  startedAtMs,
  completedAtMs,
}: Props) {
  const { t } = useTranslation();
  const { blockIds, hasOlder, viewEpoch } = useTranscriptList(store);
  const live = useTranscriptLive(store);
  const citations = useTranscriptCitations(store, sessionId);
  const turns = useMemo(() => groupTranscriptTurns(blockIds, store.getBlockSnapshot), [store, blockIds]);
  const turnAnchors = turns.map((turn) => store.getBlockSnapshot(turn.userBlockId ?? turn.processIds[0] ?? turn.answerIds[0])?.orderKey.sourceSequence ?? "");
  const workTimes = useTranscriptTurnMetadata(sessionId, turnAnchors.filter(Boolean).join(","), running);
  const scrollRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const spacerRef = useRef<HTMLDivElement>(null);
  const controllerRef = useRef<ReturnType<typeof createMessageScroll> | null>(null);
  const lastSentRef = useRef<number | null>(null);
  const submittedTurnsRef = useRef(new Map<string, { key: string; startedAtMs: number }>());
  const pendingAnchorRef = useRef<{ userId: string | null } | null>(null);
  const userScrollInputRef = useRef(0);
  const observedScrollTopRef = useRef(0);
  const scrollbarDragRef = useRef(false);
  const touchYRef = useRef<number | null>(null);
  const upwardIntentRef = useRef(false);
  const loadingOlderRef = useRef(false);
  const anchorRef = useRef<{ blockId: string; offset: number } | null>(null);
  const recoveryAnchorRef = useRef<{ blockId: string; offset: number } | null>(null);
  const readingAnchorRef = useRef<{
    element: HTMLElement;
    offset: number;
    detail: Element | null;
    closingContentEnd: number | null;
  } | null>(null);
  const readingPaddingRef = useRef(0);
  const controllerPaddingRef = useRef(0);
  const renderedViewEpochRef = useRef(viewEpoch);
  const followingLatestRef = useRef(true);
  const previousCountRef = useRef(blockIds.length);
  const setProgrammaticScrollTop = useCallback((element: HTMLElement, top: number) => {
    element.scrollTop = top;
    observedScrollTopRef.current = element.scrollTop;
  }, []);
  const [followingLatest, setFollowingLatest] = useState(true);
  const [latestContentVisible, setLatestContentVisible] = useState(false);
  const observeReadingLatest = useCallback(() => {
    const element = scrollRef.current;
    const content = contentRef.current;
    if (!element || !content) return;
    // Pausing auto-follow preserves a reading anchor; it does not mean the
    // latest reply is unseen. Observe natural content, excluding tail padding,
    // and tolerate fractional scroll positions just as the scroll controller does.
    setLatestContentVisible(element.clientHeight > 0
      && measuredContentEnd(element, content) <= element.scrollTop + element.clientHeight + 2);
  }, []);
  useEffect(() => { if (!loadingHistory) onReadingLatestChange?.(sessionId, latestContentVisible); }, [latestContentVisible, loadingHistory, onReadingLatestChange, sessionId]);
  useEffect(() => {
    if (shouldReleaseLoadedHistory(historyMemoryPressure, followingLatest, loadingOlderHistory)) onReleaseLoadedHistory?.();
  }, [historyMemoryPressure, followingLatest, loadingOlderHistory, onReleaseLoadedHistory]);
  const resetIdentity = sessionId ?? "";
  const userTurns = turns.filter((turn) => turn.userBlockId);
  const lastUserId = userTurns[userTurns.length - 1]?.userBlockId ?? null;
  const pendingVisible = pendingUserMessage
    && (pendingUserMessage.sessionId === null || pendingUserMessage.sessionId === sessionId)
    && !hasCommittedPendingMessage(pendingUserMessage, sessionId, blockIds, store.getBlockSnapshot);
  const pendingTerminal = pendingUserMessage?.status === "completed" || pendingUserMessage?.status === "failed" || pendingUserMessage?.status === "cancelled";
  const renderedTurns = pendingVisible
    ? [...turns, { id: "pending:user", userBlockId: null, processIds: [], answerIds: [] }]
    : turns;

  useImperativeHandle(ref, () => ({
    preserveReadingPosition() {
      const viewport = scrollRef.current;
      const content = contentRef.current;
      if (!viewport?.clientHeight || !content || followingLatestRef.current
        || readingAnchorRef.current?.element.isConnected || recoveryAnchorRef.current) return;
      const bounds = viewport.getBoundingClientRect();
      const firstVisible = (elements: NodeListOf<HTMLElement>) => Array.from(elements).find(element => {
        const rect = element.getBoundingClientRect();
        return rect.height > 0 && rect.bottom > bounds.top && rect.top < bounds.bottom;
      });
      const block = firstVisible(content.querySelectorAll<HTMLElement>("[data-block-id]"));
      if (!block) return;
      // Anchor to the visible paragraph rather than the top of a long answer.
      const element = firstVisible(block.querySelectorAll<HTMLElement>("p, li, pre, table, blockquote")) ?? block;
      readingAnchorRef.current = {
        element,
        offset: element.getBoundingClientRect().top - bounds.top,
        detail: null,
        closingContentEnd: null,
      };
    },
  }), []);

  const applyTailPadding = useCallback(() => {
    if (spacerRef.current) spacerRef.current.style.height = `${Math.max(controllerPaddingRef.current, readingPaddingRef.current)}px`;
  }, []);

  const reclaimReadingPadding = useCallback(() => {
    const element = scrollRef.current;
    const content = contentRef.current;
    if (!element || !content) return;
    // Reclaim only below the reader's viewport, never by clamping their position.
    readingPaddingRef.current = Math.min(readingPaddingRef.current,
      Math.max(0, element.scrollTop + element.clientHeight - measuredContentEnd(element, content)));
    applyTailPadding();
  }, [applyTailPadding]);

  const restoreReadingAnchor = useCallback(() => {
    const element = scrollRef.current;
    const content = contentRef.current;
    const anchor = readingAnchorRef.current;
    if (!element || !content) return;
    if (!anchor?.element.isConnected) {
      readingAnchorRef.current = null;
      reclaimReadingPadding();
      return;
    }
    const top = element.scrollTop + anchor.element.getBoundingClientRect().top
      - element.getBoundingClientRect().top - anchor.offset;
    if (!anchor.detail?.getAnimations().some(animation => animation.playState === "running")) {
      anchor.closingContentEnd = null;
    }
    const contentEnd = measuredContentEnd(element, content);
    readingPaddingRef.current = Math.max(0, top + element.clientHeight
      - Math.min(contentEnd, anchor.closingContentEnd ?? contentEnd));
    applyTailPadding();
    setProgrammaticScrollTop(element, top);
  }, [applyTailPadding, reclaimReadingPadding, setProgrammaticScrollTop]);

  const getController = useCallback(() => {
    controllerRef.current ??= createMessageScroll({
      measure: () => {
        const element = scrollRef.current;
        const content = contentRef.current;
        if (!element || !content) return null;
        const top = element.getBoundingClientRect().top;
        const anchors = content.querySelectorAll<HTMLElement>("[data-send-anchor]");
        const anchor = anchors[anchors.length - 1];
        return { height: element.clientHeight, scrollTop: element.scrollTop,
          contentEnd: measuredContentEnd(element, content),
          anchorTop: anchor ? anchor.getBoundingClientRect().top - top + element.scrollTop : 0 };
      },
      setPadding: (value) => { controllerPaddingRef.current = value; applyTailPadding(); },
      scrollTo: (top) => { if (scrollRef.current) setProgrammaticScrollTop(scrollRef.current, top); },
      requestFrame: (callback) => requestAnimationFrame(callback),
      cancelFrame: (id) => cancelAnimationFrame(id),
      reducedMotion: () => window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false,
      onFollowingChange: (value) => {
        followingLatestRef.current = value;
        if (value) {
          recoveryAnchorRef.current = null;
          readingAnchorRef.current = null;
          readingPaddingRef.current = 0;
          applyTailPadding();
        }
        setFollowingLatest(value);
      },
    });
    return controllerRef.current;
  }, [applyTailPadding, setProgrammaticScrollTop]);
  const pendingStartedAt = pendingUserMessage?.startedAtMs;
  const pendingTurnKey = pendingStartedAt === undefined ? null : `pending:${pendingStartedAt}`;
  const pendingRunKey = pendingUserMessage?.agentRunId && pendingUserMessage.sessionId
    ? `${pendingUserMessage.sessionId}:${pendingUserMessage.agentRunId}` : null;

  // Admission changes the input's identity, not the work period that began on send.
  // Retain its local clock only for loaded turns and the current submission.
  useLayoutEffect(() => {
    if (pendingRunKey && pendingTurnKey && pendingStartedAt !== undefined) {
      submittedTurnsRef.current.set(pendingRunKey, { key: pendingTurnKey, startedAtMs: pendingStartedAt });
    }
    const loaded = new Set(turns.flatMap((turn) => {
      const runId = turn.userBlockId && store.getBlockSnapshot(turn.userBlockId)?.presentation?.agentRunId;
      return runId ? [`${sessionId}:${runId}`] : [];
    }));
    if (pendingRunKey) loaded.add(pendingRunKey);
    for (const key of submittedTurnsRef.current.keys()) {
      if (!loaded.has(key)) submittedTurnsRef.current.delete(key);
    }
  }, [pendingRunKey, pendingTurnKey, pendingStartedAt, sessionId, store, turns]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: only identity changes reset scrolling; pending admission must preserve the anchor.
  useLayoutEffect(() => {
    void resetIdentity;
    anchorRef.current = null;
    recoveryAnchorRef.current = null;
    readingAnchorRef.current = null;
    upwardIntentRef.current = false;
    if (pendingStartedAt === undefined) {
      getController().reset();
      getController().update();
    }
    // A newly created session can acquire its durable ID during this send.
    // Preserve its ongoing animation until the pending message is admitted.
  }, [resetIdentity, getController]);

  useLayoutEffect(() => {
    if (pendingStartedAt !== undefined && lastSentRef.current !== pendingStartedAt) {
      lastSentRef.current = pendingStartedAt;
      pendingAnchorRef.current = { userId: lastUserId };
      getController().anchor();
    } else if (pendingStartedAt === undefined && pendingAnchorRef.current) {
      if (pendingAnchorRef.current.userId === lastUserId) getController().jump();
      else getController().update();
      pendingAnchorRef.current = null;
    }
  }, [pendingStartedAt, lastUserId, getController]);

  // Managed content is published before row/list subscribers render. Capture
  // the old DOM against the new authoritative identity while it is still visible.
  useLayoutEffect(() => {
    const unsubscribe = store.subscribeManagedContent(() => {
      const snapshot = store.getListSnapshot();
      if (snapshot.sessionId !== sessionId || snapshot.viewEpoch === renderedViewEpochRef.current || followingLatestRef.current) return;
      readingAnchorRef.current = null;
      const element = scrollRef.current;
      if (!element) return;
      const bounds = element.getBoundingClientRect();
      const anchor = [...element.querySelectorAll<HTMLElement>("[data-block-id]")].find((node) => {
        const rect = node.getBoundingClientRect();
        return rect.bottom > bounds.top && rect.top < bounds.bottom
          && store.getBlockSnapshot(node.dataset.blockId ?? "") !== null;
      });
      recoveryAnchorRef.current = anchor ? {
        blockId: anchor.dataset.blockId ?? "",
        offset: anchor.getBoundingClientRect().top - bounds.top,
      } : null;
    });
    return () => { unsubscribe(); };
  }, [store, sessionId]);

  const restoreRecoveryAnchor = useCallback(() => {
    const element = scrollRef.current;
    const anchor = recoveryAnchorRef.current;
    if (!element || !anchor) return;
    const node = element.querySelector(`[data-block-id="${CSS.escape(anchor.blockId)}"]`);
    if (node) setProgrammaticScrollTop(element, element.scrollTop + node.getBoundingClientRect().top
      - element.getBoundingClientRect().top - anchor.offset);
  }, [setProgrammaticScrollTop]);

  useLayoutEffect(() => {
    if (renderedViewEpochRef.current === viewEpoch) return;
    renderedViewEpochRef.current = viewEpoch;
    anchorRef.current = null;
    restoreRecoveryAnchor();
    getController().update();
  }, [viewEpoch, restoreRecoveryAnchor, getController]);

  useLayoutEffect(() => {
    const element = scrollRef.current;
    const anchor = anchorRef.current;
    if (!element || previousCountRef.current === blockIds.length) return;
    previousCountRef.current = blockIds.length;
    if (anchor !== null) {
      const node = element.querySelector(`[data-block-id="${CSS.escape(anchor.blockId)}"]`);
      if (node !== null) {
        setProgrammaticScrollTop(element, element.scrollTop + node.getBoundingClientRect().top
          - element.getBoundingClientRect().top - anchor.offset);
      }
      anchorRef.current = null;
    } else getController().update();
  }, [blockIds, getController, setProgrammaticScrollTop]);

  // Read visibility follows the committed layout, independently of whether
  // anchoring, streamed content or a disclosure caused that layout to change.
  useLayoutEffect(observeReadingLatest);

  useEffect(() => {
    const content = contentRef.current;
    const element = scrollRef.current;
    if (!content || !element) return undefined;
    const observer = new ResizeObserver(() => {
      // Deferred content layout can finish after the replacement commit.
      // Retain the recovered pixel anchor until the reader provides new input.
      restoreRecoveryAnchor();
      restoreReadingAnchor();
      getController().update();
      observeReadingLatest();
    });
    observer.observe(content);
    observer.observe(element);
    return () => { observer.disconnect(); controllerRef.current?.dispose(); };
  }, [getController, restoreRecoveryAnchor, restoreReadingAnchor, observeReadingLatest]);

  async function requestOlder() {
    const element = scrollRef.current;
    if (!element || !hasOlder || loadingOlderHistory || loadingOlderRef.current) return;
    const listTop = element.getBoundingClientRect().top;
    const anchor = [...element.querySelectorAll<HTMLElement>("[data-block-id]")]
      .find((node) => node.getBoundingClientRect().bottom > listTop);
    if (anchor) {
      anchorRef.current = {
        blockId: anchor.dataset.blockId || "",
        offset: anchor.getBoundingClientRect().top - listTop,
      };
    }
    loadingOlderRef.current = true;
    try {
      await onLoadOlderHistory();
    } finally {
      loadingOlderRef.current = false;
    }
  }

  function recordUpwardIntent() {
    upwardIntentRef.current = true;
    const element = scrollRef.current;
    if (element && !olderHistoryError && shouldLoadEarlier(element.scrollTop, hasOlder, loadingOlderHistory)) {
      upwardIntentRef.current = false;
      // Loading earlier content is an actual reading action even at the top
      // boundary. Preserve that anchor; other input waits for real movement.
      getController().pause();
      void requestOlder();
    }
  }

  function handleScroll() {
    const element = scrollRef.current;
    if (!element) return;
    const previousScrollTop = observedScrollTopRef.current;
    observedScrollTopRef.current = element.scrollTop;
    observeReadingLatest();
    // Ignore scroll events with no recent user input (browser scroll anchoring,
    // expand/collapse layout shifts) so they never detach following by themselves.
    if (!scrollbarDragRef.current && performance.now() - userScrollInputRef.current > 150) return;
    if (element.scrollTop === previousScrollTop) return;
    const movedUpward = element.scrollTop < previousScrollTop;
    recoveryAnchorRef.current = null;
    readingAnchorRef.current = null;
    reclaimReadingPadding();
    if (movedUpward) getController().pause();
    else getController().userScroll();
    if (!olderHistoryError && shouldLoadEarlier(element.scrollTop, hasOlder, loadingOlderHistory) && upwardIntentRef.current) {
      upwardIntentRef.current = false;
      void requestOlder();
    }
  }

  function scrollToLatest() {
    getController().jump();
  }

  function recordScrollInput() {
    // A passive wheel handler can run after Chromium already moved the scroll
    // offset. Keep the last observed/programmatic baseline until scroll arrives.
    userScrollInputRef.current = performance.now();
  }

  return (
    <div className="workspaceAgentRunList">
      <div
        className="workspaceMessages"
        ref={scrollRef}
        onScroll={handleScroll}
        onClickCapture={(event) => {
          const button = event.target instanceof Element ? event.target.closest<HTMLElement>("button[aria-expanded]") : null;
          const element = scrollRef.current;
          const content = contentRef.current;
          if (!button || !element || !content) return;
          recoveryAnchorRef.current = null;
          // Button activation is not reader scrolling. Ignore the layout's own
          // scroll events, including keyboard activation's recent input window.
          userScrollInputRef.current = Number.NEGATIVE_INFINITY;
          const detail = button.nextElementSibling;
          const contentEnd = measuredContentEnd(element, content);
          // Include the detail's surrounding margins/grid gap: display:none
          // removes that occupied space along with the animated height.
          const childMargins = detail ? [detail.firstElementChild, detail.lastElementChild]
            .map((child, index) => child ? Math.max(0, parseFloat(getComputedStyle(child)[index === 0 ? "marginTop" : "marginBottom"]) || 0) : 0) : [];
          const detailExtent = detail ? Math.max(detail.getBoundingClientRect().height + childMargins.reduce((sum, margin) => sum + margin, 0),
            (button.parentElement?.getBoundingClientRect().bottom ?? button.getBoundingClientRect().bottom)
              - button.getBoundingClientRect().bottom) : 0;
          const closingContentEnd = button.getAttribute("aria-expanded") === "true"
            ? contentEnd - detailExtent : null;
          readingAnchorRef.current = { element: button,
            offset: button.getBoundingClientRect().top - element.getBoundingClientRect().top,
            detail, closingContentEnd };
          // Reserve the closed layout before Chrome can shrink its scroll range.
          readingPaddingRef.current = Math.max(readingPaddingRef.current,
            element.scrollTop + element.clientHeight - (closingContentEnd ?? contentEnd), 0);
          applyTailPadding();
          getController().pause();
        }}
        onPointerDown={(event) => {
          if (!(event.target as HTMLElement).closest("button, a, [role=button]")) {
            scrollbarDragRef.current = true;
            recordScrollInput();
          }
        }}
        onPointerUp={() => { scrollbarDragRef.current = false; }}
        onPointerCancel={() => { scrollbarDragRef.current = false; }}
        onWheel={(event) => {
          if (!event.deltaY || innerCanScroll(event.target, scrollRef.current, event.deltaY)) return;
          recordScrollInput();
          if (event.deltaY < 0) recordUpwardIntent();
        }}
        onTouchStart={(event) => { touchYRef.current = event.touches[0]?.clientY ?? null; }}
        onTouchMove={(event) => {
          const nextY = event.touches[0]?.clientY ?? null;
          const previousY = touchYRef.current;
          touchYRef.current = nextY;
          if (nextY !== null && previousY !== null && nextY !== previousY
            && !innerCanScroll(event.target, scrollRef.current, previousY - nextY)) {
            recordScrollInput();
            if (nextY > previousY) recordUpwardIntent();
          }
        }}
        onTouchEnd={() => { touchYRef.current = null; }}
        onKeyDown={(event) => {
          if (event.target instanceof Element && event.target.closest("button, [role=button]")
            && ["Enter", " "].includes(event.key)) return;
          if (!["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End", " "].includes(event.key)) return;
          const upward = ["ArrowUp", "PageUp", "Home"].includes(event.key) || (event.key === " " && event.shiftKey);
          if (innerCanScroll(event.target, scrollRef.current, upward ? -1 : 1)) return;
          recordScrollInput();
          if (upward) recordUpwardIntent();
        }}
        tabIndex={0}
        role="region"
        aria-label={t("virtualAgentRunList.conversationMessages")}
      >
        {!loadingHistory && blockIds.length === 0 && live === null && !pendingVisible ? (emptyState || null) : null}
        {citations.error ? <div role="status" className="workspaceToolOutputStatus">
          {t("appRoute.citationRefreshFailed")}
          <button type="button" onClick={citations.retry}>{t("codePreview.retry")}</button>
        </div> : null}
        <div className="workspaceTranscriptBlocks" ref={contentRef}>
          {olderHistoryError && hasOlder ? <div className="workspaceToolOutputStatus" role="status">
            <span>{t("appRoute.unableToLoadEarlierMessagesPleaseTryAgain")}</span>
            <button type="button" disabled={loadingOlderHistory} onClick={() => void requestOlder()}>{t("agentChat.retry")}</button>
          </div> : null}
          {renderedTurns.map((turn, index) => {
            const isPending = pendingVisible && index === turns.length;
            const isLast = index === renderedTurns.length - 1;
            const runId = turn.userBlockId && store.getBlockSnapshot(turn.userBlockId)?.presentation?.agentRunId;
            const runKey = runId ? `${sessionId}:${runId}` : null;
            const submitted = runKey ? submittedTurnsRef.current.get(runKey) : undefined;
            const rowKey = isPending || (runKey && runKey === pendingRunKey)
              ? pendingTurnKey : submitted?.key || `${sessionId}:${turn.id}`;
            const turnLive = isLast && !isPending ? live : null;
            const time = workTimes.get(turnAnchors[index]);
            const turnCompletedAt = time?.completedAtMs ?? (isLast ? completedAtMs : undefined);
            const runEnded = typeof turnCompletedAt === "number";
            const hasWork = isPending || submitted !== undefined || (isLast && running) || turnLive !== null || turn.processIds.length > 0
              || turn.answerIds.length > 0 || time !== undefined;
            return <div className="workspaceTranscriptTurn" data-send-anchor={turn.userBlockId || isPending ? "" : undefined} key={rowKey}>
              {isPending ? <div className="workspaceTranscriptBlock workspaceTranscriptUser" data-block-id="pending:user">
                <div className="workspaceUserMessage">{pendingUserMessage?.text}</div>
                <div className="workspaceUserMessageMeta workspaceUserMessageMetaPlaceholder" aria-hidden="true" />
              </div> : turn.userBlockId ? <TranscriptBlockRow store={store} blockId={turn.userBlockId} /> : null}
              {hasWork ? <WorkProgress key="work" running={Boolean(isPending ? !pendingTerminal : isLast && (running || (submitted && !runEnded)))} finalStarted={turn.answerIds.length > 0}
                response={turnLive ? <TranscriptLiveTail live={turnLive} mode="text" /> : null}
                startedAtMs={isPending || (runKey && runKey === pendingRunKey) ? pendingStartedAt : submitted?.startedAtMs ?? time?.startedAtMs ?? (isLast ? startedAtMs : undefined)} completedAtMs={isPending ? pendingUserMessage?.completedAtMs : turnCompletedAt}>
                {groupTranscriptBlocks(store, turn.processIds).map((entry) => entry.kind === "tool"
                  ? <TranscriptToolGroupCard store={store} blockIds={entry.blockIds} toolOperations={toolOperations} citations={citations.citations} onShowCitation={onShowCitation} runEnded={runEnded} key={`tool:${entry.blockIds[0]}`} />
                  : <TranscriptBlockRow store={store} blockId={entry.blockId} key={entry.blockId} />)}
                {turnLive ? <TranscriptLiveTail live={turnLive} mode="reasoning" /> : null}
              </WorkProgress> : null}
              {turn.answerIds.map((blockId) => <TranscriptBlockRow store={store} blockId={blockId} key={blockId} />)}
              {time?.artifacts.length ? <div className="workspaceArtifactInline" aria-label={t("agentRunRow.generatedFiles")}>
                {time.artifacts.map((artifact) => <span className="workspaceArtifactInlineRow" key={artifact.artifactRef}>
                  <span className="workspaceArtifactPlus" aria-hidden="true">+</span>
                  <a id={`artifact:${artifact.artifactRef}`} href={artifact.downloadUrl} onClick={onShowArtifact ? (event) => {
                    event.preventDefault();
                    onShowArtifact(artifact, { elementId: event.currentTarget.id });
                  } : undefined}>{artifact.filename}</a>
                </span>)}
              </div> : null}
              {isLast && (isPending ? !pendingTerminal : running) ? <TranscriptActivity activity={activity} /> : null}
            </div>;
          })}
          {submissionRejected ? <TranscriptFailureNotice message={t("appRoute.messageNotSent")} /> : null}
        </div>
        <div ref={spacerRef} aria-hidden="true" style={{ height: 0, flexShrink: 0, transitionProperty: "none" }} />
      </div>
      {!followingLatest && (blockIds.length > 0 || live !== null) ? (
        <button
          type="button"
          className="workspaceJumpToLatest"
          onClick={scrollToLatest}
          aria-label={t("virtualAgentRunList.jumpToLatest")}
          title={t("virtualAgentRunList.jumpToLatest")}
        >
          <ChevronDown aria-hidden="true" />
        </button>
      ) : null}
    </div>
  );
});
