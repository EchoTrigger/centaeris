import { t } from "../../i18n";
import {
  lazy,
  memo,
  Suspense,
  type ReactNode,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useShallow } from "zustand/react/shallow";
import {
  Bot,
  Copy,
  Globe,
  ListChecks,
  Plug,
  Pencil,
  Search,
  SquareTerminal,
  type LucideIcon,
} from "lucide-react";
import { readDesktopFilePreview } from "../../lib/workspaceBridge";
import { countDiffPreviewChanges, diffPanelFileTitle } from "../../lib/diffPanel";
import {
  loadTranscriptContentRange,
} from "./transcriptContentRanges";
import { useChatViewStore } from "./chatViewStore";
import {
  getOperationPath,
  isOperationPathOpenable,
} from "./toolTimelineModel";
import {
  getToolActivityAtom,
  type ToolActivityIconToken,
} from "./toolActivityModel";
import {
  readableToolOutput,
  collectTimelineOperations,
  extractToolResultSpillContent,
  formatTimelineMeta,
  formatToolGroupTitle,
  getOperationDetailState,
  operationStatusClass,
  type OperationDetailState,
} from "./toolActivityTranscriptModel";
import type {
  AgentResultStreamProps,
  TaskStatus,
  TimelineOperation,
  TranscriptToolLikeItem,
} from "./types";

const CodePreview = lazy(() => import("../CodePreview"));

const toolActivityIconByToken: Record<ToolActivityIconToken, LucideIcon> = {
  edit: Pencil,
  command: SquareTerminal,
  webSearch: Globe,
  read: Search,
  agent: Bot,
  taskOutput: ListChecks,
  externalTool: Plug,
};

const copyToolDetailText = (text: string): void => {
  if (
    !text.trim() ||
    typeof navigator === "undefined" ||
    !navigator.clipboard
  ) {
    return;
  }
  void navigator.clipboard.writeText(text).catch(() => {
    // Clipboard failures should not disturb tool detail rendering.
  });
};

const ToolCopyButton = ({ text, label }: { text: string; label: string }) => (
  <button type="button" className="agent-tool-copy-button" aria-label={label} title={label}
    onClick={() => copyToolDetailText(text)}>
    <Copy size={14} aria-hidden="true" />
  </button>
);

const ToolResultOutput = ({ operation, variant = "output", path }: {
  operation: TimelineOperation;
  variant?: "output" | "file";
  path?: string;
}) => {
  const fallback = operation.fullOutputPath
    ? "Loading complete output…"
    : operation.modelContent || operation.outputPreview || "";
  const [rawOutput, setRawOutput] = useState(false);
  const [content, setContent] = useState(() => readableToolOutput({ contentStartByte: operation.contentStartByte, contentByteLength: operation.contentByteLength }, fallback));
  const startOffset = String(rawOutput ? 0 : operation.contentStartByte ?? 0);
  const contentEnd = !rawOutput && operation.contentStartByte !== undefined && operation.contentByteLength !== undefined
    ? operation.contentStartByte + operation.contentByteLength : Infinity;
  const activeRead = useRef(0);
  const [offsets, setOffsets] = useState(["0"]);
  const [pageIndex, setPageIndex] = useState(0);
  const [readError, setReadError] = useState(false);
  const retryPage = useRef(0);
  const [nextOffset, setNextOffset] = useState("0");
  const [hasMore, setHasMore] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);

  useEffect(() => {
    const epoch = ++activeRead.current;
    setOffsets([startOffset]);
    setPageIndex(0);
    setReadError(false);
    retryPage.current = 0;
    const reference = operation.transcriptContentRef;
    const sessionId = operation.transcriptSessionId;
    const projectionGeneration = operation.transcriptProjectionGeneration;
    if (reference && sessionId && projectionGeneration) {
      let active = true;
      setContent("");
      setLoadingMore(true);
      setHasMore(false);
      setNextOffset("0");
      void loadTranscriptContentRange({ sessionId, projectionGeneration, reference }, startOffset)
        .then((page) => {
          if (!active) return;
          setContent(readableToolOutput({ contentStartByte: operation.contentStartByte, contentByteLength: operation.contentByteLength }, page.content, Number(page.startOffset), rawOutput));
          setNextOffset(page.endOffset);
          setHasMore(page.hasMore && Number(page.endOffset) < contentEnd);
        })
        .catch(() => {
          if (active) setReadError(true);
        })
        .finally(() => { if (active) setLoadingMore(false); });
      return () => { active = false; if (activeRead.current === epoch) activeRead.current++; };
    }
    const path = operation.fullOutputPath;
    const start = operation.outputStartByte;
    const length = operation.outputByteLength;
    if (!path || start === undefined || length === undefined) {
      setContent(readableToolOutput({ contentStartByte: operation.contentStartByte, contentByteLength: operation.contentByteLength }, operation.modelContent || operation.outputPreview || "", 0, rawOutput));
      setHasMore(false);
      return;
    }
    let active = true;
    void readDesktopFilePreview(path)
      .then((response) => {
        if (active) {
          setContent(readableToolOutput({ contentStartByte: operation.contentStartByte, contentByteLength: operation.contentByteLength }, extractToolResultSpillContent(response.content, start, length), 0, rawOutput));
        }
      })
      .catch(() => {
        if (active) {
          const preview = operation.modelContent || operation.outputPreview || "";
          setContent(readableToolOutput({ contentStartByte: operation.contentStartByte, contentByteLength: operation.contentByteLength }, preview, 0, rawOutput));
        }
      });
    return () => {
      active = false;
    };
  }, [
    rawOutput,
    startOffset,
    contentEnd,
    operation.contentStartByte,
    operation.contentByteLength,
    operation.fullOutputPath,
    operation.modelContent,
    operation.outputByteLength,
    operation.outputPreview,
    operation.outputStartByte,
    operation.transcriptContentRef,
    operation.transcriptProjectionGeneration,
    operation.transcriptSessionId,
  ]);

  async function loadMore(targetIndex = pageIndex + 1) {
    const reference = operation.transcriptContentRef;
    const sessionId = operation.transcriptSessionId;
    const projectionGeneration = operation.transcriptProjectionGeneration;
    if (!reference || !sessionId || !projectionGeneration || loadingMore) return;
    const epoch = activeRead.current;
    retryPage.current = targetIndex;
    const offset = offsets[targetIndex] ?? nextOffset;
    setLoadingMore(true);
    setReadError(false);
    try {
      const page = await loadTranscriptContentRange(
        { sessionId, projectionGeneration, reference },
        offset,
      );
      if (activeRead.current !== epoch) return;
      setContent(readableToolOutput({ contentStartByte: operation.contentStartByte, contentByteLength: operation.contentByteLength }, page.content, Number(page.startOffset), rawOutput));
      setOffsets((current) => { const next = [...current]; next[targetIndex] = offset; return next; });
      setPageIndex(targetIndex);
      setNextOffset(page.endOffset);
      setHasMore(page.hasMore && Number(page.endOffset) < contentEnd);
    } catch {
      if (activeRead.current === epoch) setReadError(true);
    } finally {
      if (activeRead.current === epoch) setLoadingMore(false);
    }
  }

  if (!content && !loadingMore && !readError && operation.contentStartByte === undefined) return null;
  return (
    <section className={`agent-tool-detail-section ${variant === "file" ? "is-file" : "is-output"}`}
      aria-label={variant === "file" ? "Read result" : "Output"}>
      <div className="agent-tool-detail-section-header">
        <span className="agent-tool-detail-section-title" title={path}>{variant === "file" ? path || "Read result" : "Output"}</span>
        <div className="agent-tool-detail-actions">
          {operation.contentStartByte !== undefined ? <button type="button" className="agent-tool-raw-toggle" onClick={() => setRawOutput(value => !value)}>{rawOutput ? "Readable output" : "Raw output"}</button> : null}
          {content ? <ToolCopyButton text={content} label={variant === "file" ? "Copy current content page" : "Copy current output page"} /> : null}
        </div>
      </div>
      {content ? <div className="agent-tool-output-viewport" tabIndex={0} role="region" aria-label="Tool output"><pre className="agent-tool-bash-output">{content}</pre></div> : null}
      {!content && loadingMore ? <span role="status">{t("transcriptText.loading")}</span> : null}
      {readError ? <span role="alert">{t("transcriptText.failed")}
        <button type="button" onClick={() => { void loadMore(retryPage.current); }}>{t("transcriptText.retry")}</button>
      </span> : null}
      {pageIndex > 0 || hasMore ? <div className="agent-tool-detail-paging">
        {pageIndex > 0 ? <button type="button" disabled={loadingMore} onClick={() => { void loadMore(pageIndex - 1); }}>
          {t("transcriptText.previousOutput")}
        </button> : null}
        {hasMore ? (
          <button type="button" onClick={() => { void loadMore(); }} disabled={loadingMore}>
            {loadingMore
              ? t("toolActivityTranscript.loading")
              : t("transcriptText.nextOutput")}
          </button>
        ) : null}
      </div> : null}
    </section>
  );
};

const renderOperationDetail = (
  operation: TimelineOperation,
  detailState: OperationDetailState,
  statusClassName: TaskStatus,
): ReactNode => {
  const { command, path, hasBashDetail, hasEditDetail, hasTextDetail } = detailState;
  const diff = operation.diffPreview;
  const changeCount = diff ? countDiffPreviewChanges(diff) : null;
  return (
    <div className={`agent-operation-body agent-tool-node-body ${statusClassName === "running" ? "is-running" : "is-done"}`}>
      <div className="agent-tool-command-card agent-tool-detail-card">
        {hasBashDetail ? <>
          <div className="agent-tool-detail-title">Shell</div>
          {command ? <section className="agent-tool-detail-section is-command" aria-label="Command">
            <div className="agent-tool-detail-section-header">
              <span className="agent-tool-detail-section-title">Command</span>
              <ToolCopyButton text={command} label="Copy command" />
            </div>
            <div className="agent-tool-command-viewport" tabIndex={0}>
              <span className="agent-tool-command-prompt" aria-hidden="true">$</span>
              <pre className="agent-tool-bash-command">{command}</pre>
            </div>
          </section> : null}
          <ToolResultOutput operation={operation} />
          {operation.error ? <pre className="agent-tool-output-block is-error">{operation.error}</pre> : null}
        </> : null}
        {hasEditDetail && diff ? (
          <section className="agent-tool-detail-section is-diff" aria-label="File changes">
            <div className="agent-tool-detail-section-header">
              <span className="agent-tool-detail-section-title" title={path}>{diffPanelFileTitle(path || "changes.diff")}</span>
              {changeCount ? <span className="agent-tool-diff-counts" aria-label={`${changeCount.added} added, ${changeCount.removed} removed`}>
                <span className="is-added">+{changeCount.added}</span> <span className="is-removed">-{changeCount.removed}</span>
              </span> : null}
              <ToolCopyButton text={diff} label="Copy diff" />
            </div>
            <div className="agent-tool-diff-preview">
              <Suspense
                fallback={
                  <div className="agent-tool-empty-output">Loading diff...</div>
                }
              >
                <CodePreview
                  content={diff}
                  path={path || "changes.diff"}
                  variant="diff"
                />
              </Suspense>
            </div>
          </section>
        ) : null}
        {hasEditDetail && !diff && operation.error ? (
          <pre className="agent-tool-output-block is-error">{operation.error}</pre>
        ) : null}
        {hasTextDetail ? <ToolResultOutput operation={operation}
          variant={operation.toolName === "read" ? "file" : "output"} path={path} /> : null}
      </div>
    </div>
  );
};

const ToolOperationNode = memo(function ToolOperationNode({
  operation,
  operationId,
  onOpenWorkspacePath,
}: {
  operation: TimelineOperation;
  operationId: string;
  onOpenWorkspacePath?: AgentResultStreamProps["onOpenWorkspacePath"];
}) {
  const isOpen = useChatViewStore(state => Boolean(state.expandedTools[operationId]));
  const onToggle = () => useChatViewStore.setState(state => {
    const open = !state.expandedTools[operationId];
    return { expandedTools: { ...state.expandedTools, [operationId]: open } };
  });
  const statusClassName = operationStatusClass(operation.status);
  const atom = getToolActivityAtom(operation);
  const path = getOperationPath(operation);
  const metaText = formatTimelineMeta(operation);
  const detailState = getOperationDetailState(operation, atom, statusClassName);
  const hasDetail =
    detailState.hasBashDetail ||
    detailState.hasEditDetail ||
    detailState.hasTextDetail;
  const leafSummary = [formatToolGroupTitle([operation]), metaText].filter(Boolean).join(" · ");
  const ActivityIcon = toolActivityIconByToken[atom.iconToken];
  const summary = (
    <>
      <ActivityIcon className="agent-tool-node-icon" aria-hidden="true" />
      <span
        className="agent-tool-node-action is-inline-summary"
        title={leafSummary}
      >
        {leafSummary}
      </span>
      {hasDetail ? (
        <span
          className={`agent-operation-chevron ${isOpen ? "open" : ""}`}
          aria-hidden="true"
        />
      ) : null}
    </>
  );

  if (!hasDetail) {
    if (
      atom.pathOpenable &&
      isOperationPathOpenable(operation) &&
      path &&
      onOpenWorkspacePath
    ) {
      return (
        <div
          className={`agent-operation-group agent-tool-node ${statusClassName}`}
          key={operationId}
        >
          <button
            type="button"
            className="agent-operation-summary agent-tool-node-summary is-path-link"
            aria-label={t("toolActivityTranscript.openValue", { value1: path })}
            onClick={() =>
              onOpenWorkspacePath(path, {
                startLine: operation.startLine,
                endLine: operation.endLine,
                taskId: operation.taskId,
              })
            }
          >
            {summary}
          </button>
        </div>
      );
    }
    return (
      <div
        className={`agent-operation-group agent-tool-node ${statusClassName}`}
        key={operationId}
      >
        <div className="agent-operation-summary agent-tool-node-summary">
          {summary}
        </div>
      </div>
    );
  }

  return (
    <div
      className={`agent-operation-group agent-tool-node ${statusClassName}`}
      key={operationId}
    >
      <button type="button" aria-expanded={isOpen}
        className="agent-operation-summary agent-tool-node-summary"
        onClick={(event) => {
          event.preventDefault();
          onToggle();
        }}
      >
        {summary}
      </button>
      {isOpen ? <OperationDetail operation={operation} detailState={detailState} statusClassName={statusClassName} /> : null}
    </div>
  );
});

const OperationDetail = ({ operation, detailState, statusClassName }: { operation: TimelineOperation; detailState: OperationDetailState; statusClassName: TaskStatus }) => renderOperationDetail(operation, detailState, statusClassName);

export const TaskGroupTranscriptItem = memo(function TaskGroupTranscriptItem({
  entry,
  onOpenWorkspacePath,
}: {
  entry: TranscriptToolLikeItem;
  onOpenWorkspacePath?: AgentResultStreamProps["onOpenWorkspacePath"];
}) {
  const taskIds = useMemo(() => entry.tasks.map((task) => task.id), [entry.tasks]);
  const tasks = useChatViewStore(
    useShallow((state) =>
      taskIds.map((taskId, index) => state.taskById[taskId] ?? entry.tasks[index]),
    ),
  );
  const operations = useMemo(() => collectTimelineOperations(tasks), [tasks]);
  return (
    <div className="agent-tool-node-list" data-waterfall-section={entry.waterfall?.section ?? "tool"}>
      {operations.map(operation => {
        const operationId = `operation:${operation.taskId}:${operation.callId}`;
        return <ToolOperationNode key={operationId} operation={operation} operationId={operationId} onOpenWorkspacePath={onOpenWorkspacePath} />;
      })}
    </div>
  );
}, (previous, next) =>
  previous.entry.id === next.entry.id &&
  previous.onOpenWorkspacePath === next.onOpenWorkspacePath &&
  previous.entry.tasks.length === next.entry.tasks.length &&
  previous.entry.tasks.every((task, index) => task === next.entry.tasks[index]),
);
