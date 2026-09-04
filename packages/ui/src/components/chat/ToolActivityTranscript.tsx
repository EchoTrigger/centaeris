import {
  lazy,
  memo,
  Suspense,
  type ReactNode,
  useEffect,
  useMemo,
  useState,
} from "react";
import { useShallow } from "zustand/react/shallow";
import {
  Bot,
  Copy,
  FilePenLine,
  FileText,
  ListChecks,
  Plug,
  Search,
  Terminal,
  type LucideIcon,
} from "lucide-react";
import { readDesktopFilePreview } from "../../lib/workspaceBridge";
import { useChatViewStore } from "./chatViewStore";
import {
  formatFullCommandLine,
  getOperationPath,
  isOperationPathOpenable,
} from "./toolTimelineModel";
import {
  getToolActivityAtom,
  getToolActivityPresentation,
  type ToolActivityIconToken,
} from "./toolActivityModel";
import {
  collectTimelineOperations,
  extractToolResultSpillContent,
  formatOperationDuration,
  formatOperationInlineSummary,
  formatTimelineMeta,
  formatToolGroupTitle,
  getOperationDetailState,
  operationBashStatusLabel,
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
  edit: FilePenLine,
  command: Terminal,
  webSearch: Search,
  read: FileText,
  agent: Bot,
  taskOutput: ListChecks,
  externalTool: Plug,
};

const copyToolDetailText = (text: string): void => {
  const normalized = text.trim();
  if (
    !normalized ||
    typeof navigator === "undefined" ||
    !navigator.clipboard
  ) {
    return;
  }
  void navigator.clipboard.writeText(normalized).catch(() => {
    // Clipboard failures should not disturb tool detail rendering.
  });
};

const ToolResultOutput = ({ operation }: { operation: TimelineOperation }) => {
  const fallback = operation.fullOutputPath
    ? "Loading complete output…"
    : operation.modelContent || operation.outputPreview || "";
  const [content, setContent] = useState(fallback);

  useEffect(() => {
    const path = operation.fullOutputPath;
    const start = operation.outputStartByte;
    const length = operation.outputByteLength;
    if (!path || start === undefined || length === undefined) {
      setContent(operation.modelContent || operation.outputPreview || "");
      return;
    }
    let active = true;
    void readDesktopFilePreview(path)
      .then((response) => {
        if (active) {
          setContent(extractToolResultSpillContent(response.content, start, length));
        }
      })
      .catch(() => {
        if (active) {
          const preview = operation.modelContent || operation.outputPreview || "";
          setContent(preview.replace(/\n\n\[Full tool result:[\s\S]*$/, ""));
        }
      });
    return () => {
      active = false;
    };
  }, [
    operation.fullOutputPath,
    operation.modelContent,
    operation.outputByteLength,
    operation.outputPreview,
    operation.outputStartByte,
  ]);

  return content ? <pre className="agent-tool-bash-output">{content}</pre> : null;
};

const renderOperationDetail = (
  operation: TimelineOperation,
  detailState: OperationDetailState,
  statusClassName: TaskStatus,
): ReactNode => {
  const {
    command,
    path,
    showBashCommandInput,
    hasBashDetail,
    hasEditDetail,
    hasTextDetail,
  } = detailState;
  return (
    <div
      className={`agent-operation-body agent-tool-node-body ${statusClassName === "running" ? "is-running" : "is-done"}`}
    >
      <div className="agent-tool-command-card">
        {hasBashDetail ? (
          <div className={`agent-tool-bash-card ${statusClassName}`}>
            <div className="agent-tool-bash-header">
              <span>Bash</span>
              {command ? (
                <button
                  type="button"
                  className="agent-tool-copy-button agent-tool-bash-copy-button"
                  onClick={() => copyToolDetailText(command)}
                  aria-label="Copy command"
                  title="Copy command"
                >
                  <Copy className="agent-tool-copy-icon" aria-hidden="true" />
                </button>
              ) : null}
            </div>
            <div className="agent-tool-bash-scroll">
              {showBashCommandInput && command ? (
                <pre className="agent-tool-bash-command">
                  {formatFullCommandLine(command)}
                </pre>
              ) : null}
              {operation.modelContent || operation.outputPreview || operation.fullOutputPath ? (
                <ToolResultOutput operation={operation} />
              ) : null}
              {operation.error ? (
                <pre className="agent-tool-bash-output is-error">{operation.error}</pre>
              ) : null}
            </div>
            <div className={`agent-tool-bash-status ${statusClassName}`}>
              <span>{operationBashStatusLabel(operation, statusClassName)}</span>
            </div>
          </div>
        ) : null}
        {hasEditDetail && operation.diffPreview ? (
          <div className="agent-tool-command-section">
            <div className="agent-tool-command-label">Diff</div>
            <div className="agent-tool-diff-preview">
              <Suspense
                fallback={
                  <div className="agent-tool-empty-output">Loading diff...</div>
                }
              >
                <CodePreview
                  content={operation.diffPreview}
                  path={path || "changes.diff"}
                  variant="diff"
                />
              </Suspense>
            </div>
          </div>
        ) : null}
        {hasEditDetail && !operation.diffPreview && operation.error ? (
          <pre className="agent-tool-output-block is-error">{operation.error}</pre>
        ) : null}
        {hasTextDetail ? <ToolResultOutput operation={operation} /> : null}
      </div>
    </div>
  );
};

const renderToolOperationNode = ({
  operation,
  operationId,
  isOpen,
  onToggle,
  onOpenWorkspacePath,
}: {
  operation: TimelineOperation;
  operationId: string;
  isOpen: boolean;
  onToggle: () => void;
  onOpenWorkspacePath?: AgentResultStreamProps["onOpenWorkspacePath"];
}) => {
  const statusClassName = operationStatusClass(operation.status);
  const atom = getToolActivityAtom(operation);
  const path = getOperationPath(operation);
  const metaText = formatTimelineMeta(operation);
  const durationText = formatOperationDuration(operation);
  const detailState = getOperationDetailState(operation, atom, statusClassName);
  const hasDetail =
    detailState.hasBashDetail ||
    detailState.hasEditDetail ||
    detailState.hasTextDetail;
  const detail = isOpen
    ? renderOperationDetail(operation, detailState, statusClassName)
    : null;
  const leafSummary = formatOperationInlineSummary(
    operation,
    statusClassName,
    durationText,
    metaText,
  );
  const summary = (
    <>
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
            aria-label={`打开 ${path}`}
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
    <details
      className={`agent-operation-group agent-tool-node ${statusClassName}`}
      key={operationId}
      open={isOpen}
    >
      <summary
        className="agent-operation-summary agent-tool-node-summary"
        onClick={(event) => {
          event.preventDefault();
          onToggle();
        }}
      >
        {summary}
      </summary>
      {detail}
    </details>
  );
};

export const TaskGroupTranscriptItem = memo(function TaskGroupTranscriptItem({
  entry,
  onOpenWorkspacePath,
}: {
  entry: TranscriptToolLikeItem;
  onOpenWorkspacePath?: AgentResultStreamProps["onOpenWorkspacePath"];
}) {
  const [isOpen, setIsOpen] = useState(false);
  const [expandedOperationIds, setExpandedOperationIds] = useState<Set<string>>(
    new Set(),
  );
  const [collapsedDefaultOpenOperationIds, setCollapsedDefaultOpenOperationIds] =
    useState<Set<string>>(new Set());
  const taskIds = useMemo(() => entry.tasks.map((task) => task.id), [entry.tasks]);
  const tasks = useChatViewStore(
    useShallow((state) =>
      taskIds.map((taskId, index) => state.taskById[taskId] ?? entry.tasks[index]),
    ),
  );
  const operations = useMemo(() => collectTimelineOperations(tasks), [tasks]);
  const live = tasks.some((task) => task.status === "running");
  const presentation = useMemo(
    () => getToolActivityPresentation(operations),
    [operations],
  );
  const latestOperation = operations[operations.length - 1];
  const liveText = formatToolGroupTitle(operations);
  const iconToken = live && latestOperation
    ? getToolActivityAtom(latestOperation).iconToken
    : presentation.iconToken;
  const ActivityIcon = toolActivityIconByToken[iconToken];

  const toggleOperation = (operationId: string, defaultOpen: boolean) => {
    const setter = defaultOpen
      ? setCollapsedDefaultOpenOperationIds
      : setExpandedOperationIds;
    setter((previous) => {
      const next = new Set(previous);
      if (next.has(operationId)) {
        next.delete(operationId);
      } else {
        next.add(operationId);
      }
      return next;
    });
  };

  const renderToolTaskBody = (defaultOperationOpen: boolean): ReactNode => (
    <div
      className="agent-tool-node-list"
      data-waterfall-section={entry.waterfall?.section ?? "tool"}
    >
      {operations.map((operation, index) => {
        const operationId = `${entry.id}-operation-${operation.taskId}-${operation.toolName}-${index}`;
        const operationOpen = defaultOperationOpen
          ? !collapsedDefaultOpenOperationIds.has(operationId)
          : expandedOperationIds.has(operationId);
        return renderToolOperationNode({
          operation,
          operationId,
          isOpen: operationOpen,
          onToggle: () => toggleOperation(operationId, defaultOperationOpen),
          onOpenWorkspacePath,
        });
      })}
    </div>
  );

  const summary = (
    <div className="agent-operation-summary">
      <ActivityIcon className="agent-tool-node-icon" aria-hidden="true" />
      <span className={`agent-operation-summary-text ${live ? "agentRunStatusText" : ""}`}>{liveText}</span>
      {presentation.expandable ? (
        <span
          className={`agent-operation-chevron ${isOpen ? "open" : ""}`}
          aria-hidden="true"
        />
      ) : null}
    </div>
  );

  if (!presentation.expandable) {
    return (
      <div
        className={`agent-operation-group ${live ? "is-live" : ""}`}
        data-waterfall-section={entry.waterfall?.section ?? "tool"}
        aria-live={live ? "polite" : undefined}
      >
        {summary}
      </div>
    );
  }

  return (
    <details
      className={`agent-operation-group ${live ? "is-live" : ""}`}
      data-waterfall-section={entry.waterfall?.section ?? "tool"}
      aria-live={live ? "polite" : undefined}
      open={isOpen}
    >
      <summary
        className="agent-operation-summary agent-operation-summary-toggle"
        onClick={(event) => {
          event.preventDefault();
          setIsOpen((previous) => !previous);
        }}
      >
        <ActivityIcon className="agent-tool-node-icon" aria-hidden="true" />
        <span className="agent-operation-summary-text">{liveText}</span>
        <span
          className={`agent-operation-chevron ${isOpen ? "open" : ""}`}
          aria-hidden="true"
        />
      </summary>
      {isOpen ? renderToolTaskBody(false) : null}
    </details>
  );
}, (previous, next) =>
  previous.entry.id === next.entry.id &&
  previous.onOpenWorkspacePath === next.onOpenWorkspacePath &&
  previous.entry.tasks.length === next.entry.tasks.length &&
  previous.entry.tasks.every((task, index) => task === next.entry.tasks[index]),
);
