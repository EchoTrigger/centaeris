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
  CornerDownLeft,
  FilePenLine,
  FileText,
  ListChecks,
  Plug,
  Search,
  Terminal,
  type LucideIcon,
} from "lucide-react";
import { MarkdownContent } from "./MarkdownContent";
import { readDesktopFilePreview } from "../../lib/workspaceBridge";
import { formatDuration } from "./agentDuration";
import {
  compactText,
  getChunkWaterfallOrder,
  getChunkWaterfallSection,
  taskStatusLabel,
  toDisplayPath,
} from "./chatRuntimeModel";
import {
  runtimeEasterEgg,
  tachikomaEasterEgg,
} from "./chatRuntimeCore";
import { useChatViewStore } from "./chatViewStore";
import {
  formatOperationLineCoverage,
  formatFullCommandLine,
  getDisplayOperationsForTask,
  getOperationCommand,
  getOperationPath,
  getOperationQuery,
  isCommandOperation,
  isOperationPathOpenable,
} from "./toolTimelineModel";
import {
  getToolActivityAtom,
  getToolActivityPresentation,
  type ToolActivityAtom,
  type ToolActivityIconToken,
} from "./toolActivityModel";
import type {
  AssistantExecutionTurn,
  AgentDisplayEntry,
  AgentResultStreamProps,
  GuidedSupplementChunk,
  NarrativeChunk,
  SubagentChunk,
  SubagentResult,
  TaskChunk,
  TaskResult,
  TaskStatus,
  TimelineOperation,
  TranscriptItem,
  TranscriptProcessSection,
  TranscriptTextItem,
  TranscriptToolGroupItem,
  TranscriptToolLikeItem,
  TranscriptViewModel,
} from "./types";

const CodePreview = lazy(() => import("../CodePreview"));

const collectTimelineOperations = (
  tasks: TaskResult[],
): TimelineOperation[] => {
  const operations: TimelineOperation[] = [];
  for (const task of tasks) {
    for (const operation of getDisplayOperationsForTask(task)) {
      operations.push({
        ...operation,
        status: operation.status ?? task.status,
        taskId: task.id,
        taskTitle: task.title,
        durationMs: task.durationMs,
        normalizedInput: task.normalizedInput,
        displayTarget: task.displayTarget,
        modelContent: task.modelContent,
        fullOutputPath: task.fullOutputPath,
        outputStartByte: task.outputStartByte,
        outputByteLength: task.outputByteLength,
      });
    }
  }
  return operations.slice(0, 32);
};

const formatToolCountTitle = (
  operation: TimelineOperation,
  count: number,
  status: TaskStatus,
): string => {
  const atom = getToolActivityAtom(operation);
  if (atom.kind === "webSearch") {
    return status === "running"
      ? "Searching the web"
      : status === "error"
        ? "Search failed"
        : atom.title;
  }
  const nouns: Record<Exclude<typeof atom.kind, "webSearch">, [string, string]> = {
    command: ["a command", "commands"],
    read: ["a file", "files"],
    edit: ["a file", "files"],
    agent: ["an agent", "agents"],
    taskOutput: ["a task result", "task results"],
    externalTool: ["an external tool", "external tools"],
  };
  const [singular, plural] = nouns[atom.kind];
  if (status === "error") {
    return count === 1 ? atom.failedVerb : `${count} ${plural} failed`;
  }
  const verb = status === "running" ? atom.runningVerb : atom.completedVerb;
  return `${verb} ${count === 1 ? singular : `${count} ${plural}`}`;
};

const formatToolGroupTitle = (operations: TimelineOperation[]): string => {
  if (operations.length === 1) {
    const operation = operations[0];
    const atom = getToolActivityAtom(operation);
    const status = operationStatusClass(operation.status);
    const description = typeof operation.normalizedInput?.description === "string"
      ? operation.normalizedInput.description.trim()
      : "";
    if (isCommandOperation(operation) && description) {
      return description;
    }
    const target = toDisplayPath(getOperationPath(operation));
    if (target && ["read", "write", "edit"].includes(operation.toolName)) {
      return `${operationInlineVerb(operation, status)} ${target}`;
    }
    if (atom.kind === "webSearch") {
      return formatToolCountTitle(operation, 1, status);
    }
  }
  const groups: Array<{ operation: TimelineOperation; count: number; status: TaskStatus }> = [];
  const groupIndexByKey = new Map<string, number>();
  for (const operation of operations) {
    getToolActivityAtom(operation);
    const key = operation.toolName;
    const status = operationStatusClass(operation.status);
    const existingIndex = groupIndexByKey.get(key);
    if (existingIndex === undefined) {
      groupIndexByKey.set(key, groups.length);
      groups.push({ operation, count: 1, status });
      continue;
    }
    const group = groups[existingIndex];
    group.count += 1;
    if (status === "running" || (status === "error" && group.status === "done")) {
      group.status = status;
    }
  }
  return groups
    .map(({ operation, count, status }) => formatToolCountTitle(operation, count, status))
    .join(", ");
};

export const formatCompletedToolGroupTitle = (
  tasks: TaskResult[],
): string => {
  return formatToolGroupTitle(collectTimelineOperations(tasks));
};

export const formatRunningToolGroupTitle = (tasks: TaskResult[]): string => {
  const operations = collectTimelineOperations(tasks);
  if (operations.length === 0) {
    return compactText(tasks[tasks.length - 1]?.title || "Thinking", 180);
  }
  return formatToolGroupTitle(operations);
};

const toolActivityIconByToken: Record<ToolActivityIconToken, LucideIcon> = {
  edit: FilePenLine,
  command: Terminal,
  webSearch: Search,
  read: FileText,
  agent: Bot,
  taskOutput: ListChecks,
  externalTool: Plug,
};

const operationStatusClass = (status?: string): TaskStatus => {
  const normalized = (status || "").trim().toLowerCase();
  if (["error", "failed", "timeout", "blocked"].includes(normalized)) {
    return "error";
  }
  if (["running", "pending", "started"].includes(normalized)) {
    return "running";
  }
  return "done";
};

const formatTimelineMeta = (
  operation: TimelineOperation,
): string | undefined => {
  const lineCoverage = formatOperationLineCoverage(operation);
  if (lineCoverage) {
    return lineCoverage;
  }
  if (operation.matchCount !== undefined) {
    return `${operation.matchCount} results`;
  }
  if (operation.lines !== undefined) {
    return `${operation.lines} lines`;
  }
  if (operation.exitCode !== undefined) {
    return `exit ${operation.exitCode}`;
  }
  return undefined;
};

const operationStatusLabel = (
  operation: TimelineOperation,
  statusClassName: TaskStatus,
): string => {
  if (typeof operation.exitCode === "number") {
    return operation.exitCode === 0 ? "Succeeded" : "Failed";
  }
  return taskStatusLabel[statusClassName];
};

const operationBashStatusLabel = (
  operation: TimelineOperation,
  statusClassName: TaskStatus,
): string => {
  if (statusClassName === "done") {
    return "Succeeded";
  }
  return operationStatusLabel(operation, statusClassName);
};

const formatOperationDuration = (
  operation: TimelineOperation,
): string | undefined =>
  typeof operation.durationMs === "number"
    ? formatDuration(operation.durationMs)
    : undefined;

const operationInlineVerb = (
  operation: TimelineOperation,
  statusClassName: TaskStatus,
): string => {
  const atom = getToolActivityAtom(operation);
  if (statusClassName === "running") {
    return atom.runningVerb;
  }
  return statusClassName === "error" ? atom.failedVerb : atom.completedVerb;
};

const formatOperationInlineSummary = (
  operation: TimelineOperation,
  statusClassName: TaskStatus,
  durationText?: string,
  metaText?: string,
): string => {
  const command = isCommandOperation(operation)
    ? getOperationCommand(operation)
    : undefined;
  const displayTarget = operation.displayTarget?.trim();
  const inputDescription = typeof operation.normalizedInput?.description === "string"
    ? operation.normalizedInput.description.trim()
    : "";
  const description = inputDescription || undefined;
  if (isCommandOperation(operation) && description) {
    return [description, durationText ? `已持续 ${durationText}` : undefined]
      .filter(Boolean)
      .join("，");
  }
  const target = [
    command,
    toDisplayPath(getOperationPath(operation)),
    getOperationQuery(operation),
    operation.text,
    displayTarget,
    operation.toolName,
  ]
    .map((value) => String(value ?? "").trim())
    .find((value) => value.length > 0);
  const parts = [`${operationInlineVerb(operation, statusClassName)}${target ? ` · ${target}` : ""}`];
  if (metaText && !target?.includes(metaText)) {
    parts.push(metaText);
  }
  if (durationText) {
    parts.push(`已持续 ${durationText}`);
  }
  return parts.join("，");
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

export const extractToolResultSpillContent = (
  spill: string,
  startByte: number,
  byteLength: number,
): string => {
  const bytes = new TextEncoder().encode(spill);
  const end = startByte + byteLength;
  if (startByte < 0 || byteLength < 0 || end > bytes.length) {
    throw new Error("tool result spill is incomplete");
  }
  return new TextDecoder().decode(bytes.slice(startByte, end));
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

const getOperationDetailState = (
  operation: TimelineOperation,
  atom: ToolActivityAtom,
  statusClassName: TaskStatus,
) => {
  const command = getOperationCommand(operation);
  const path = getOperationPath(operation);
  const showBashCommandInput = statusClassName !== "running";
  const hasBashDetail =
    atom.detailRendererKind === "bash" &&
    Boolean(
      (showBashCommandInput && command) ||
      operation.outputPreview ||
      operation.modelContent ||
      operation.fullOutputPath ||
      operation.error,
    );
  const hasEditDetail =
    atom.detailRendererKind === "diff" &&
    Boolean(operation.diffPreview || operation.outputPreview || operation.error);
  const hasTextDetail =
    atom.detailRendererKind !== "bash" &&
    Boolean(
      operation.fullOutputPath ||
      (atom.detailRendererKind !== "diff" &&
        (operation.modelContent || operation.outputPreview)),
    );
  return {
    command,
    path,
    showBashCommandInput,
    hasBashDetail,
    hasEditDetail,
    hasTextDetail,
  };
};

const renderOperationDetail = (
  operation: TimelineOperation,
  detailState: ReturnType<typeof getOperationDetailState>,
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
                  onClick={() =>
                    copyToolDetailText(command || "")
                  }
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

const buildAgentDisplayEntries = (
  chunks: Array<
    NarrativeChunk | GuidedSupplementChunk | TaskChunk | SubagentChunk
  >,
): AgentDisplayEntry[] => {
  const entries: AgentDisplayEntry[] = [];
  let pendingTasks: TaskResult[] = [];

  const flushTasks = () => {
    if (pendingTasks.length === 0) {
      return;
    }
    entries.push({
      kind: "taskGroup",
      id: pendingTasks[0].id,
      tasks: pendingTasks,
    });
    pendingTasks = [];
  };

  for (const chunk of chunks) {
    if (chunk.kind === "task") {
      pendingTasks.push(chunk.task);
      continue;
    }
    flushTasks();
    if (chunk.kind === "guidedSupplement") {
      entries.push({
        kind: "guidedSupplement",
        chunk,
      });
      continue;
    }
    if (chunk.kind === "subagent") {
      entries.push({
        kind: "subagent",
        chunk,
      });
      continue;
    }
    entries.push({
      kind: "narrative",
      chunk,
    });
  }
  flushTasks();
  return entries;
};

const resolveSharedTurnId = (tasks: TaskResult[]): string | undefined => {
  const turnIds = new Set(
    tasks
      .map((task) => task.turnId?.trim())
      .filter((turnId): turnId is string => Boolean(turnId)),
  );
  return turnIds.size === 1 ? Array.from(turnIds)[0] : undefined;
};

const buildToolActivityItem = (
  entryId: string,
  tasks: TaskResult[],
): TranscriptToolGroupItem => ({
  kind: "toolGroup",
  id: `${entryId}-activity`,
  turnId: resolveSharedTurnId(tasks),
  tasks,
  waterfall: tasks.find((task) => task.waterfall)?.waterfall,
});

const buildProcessSections = (
  items: TranscriptItem[],
): TranscriptProcessSection[] => {
  const sections: TranscriptProcessSection[] = [];
  let current: TranscriptProcessSection | null = null;

  const flushCurrent = () => {
    if (!current) {
      return;
    }
    if (current.heading || current.items.length > 0) {
      sections.push(current);
    }
    current = null;
  };

  for (const item of items) {
    if (item.kind === "assistantText") {
      flushCurrent();
      current = {
        id: `section-${item.id}`,
        heading: item,
        items: [],
      };
      continue;
    }
    if (!current) {
      current = {
        id: `section-${item.id}`,
        items: [],
      };
    }
    current.items.push(item);
  }

  flushCurrent();
  return sections;
};

export const buildTranscriptProcessViewModel = (
  turn: Pick<AssistantExecutionTurn, "chunks">,
): Pick<TranscriptViewModel, "processItems" | "processSections"> => {
  const waterfallChunks = turn.chunks.filter(
    (chunk) => getChunkWaterfallSection(chunk) !== "final",
  );
  const processItems: TranscriptItem[] = [];
  for (const entry of buildAgentDisplayEntries(waterfallChunks)) {
    if (entry.kind === "narrative") {
      processItems.push({
        kind: "assistantText",
        id: entry.chunk.id,
        phase: entry.chunk.phase === "compaction" ? "compaction" : "process",
        text: entry.chunk.text,
        tone: entry.chunk.tone,
        turnId: entry.chunk.turnId,
        waterfall: entry.chunk.waterfall,
      });
      continue;
    }
    if (entry.kind === "guidedSupplement") {
      processItems.push({
        kind: "guidedSupplement",
        id: entry.chunk.id,
        text: entry.chunk.text,
        timestamp: entry.chunk.timestamp,
        waterfall: entry.chunk.waterfall,
      });
      continue;
    }
    if (entry.kind === "subagent") {
      continue;
    }
    const orderedTasks = [...entry.tasks].sort((left, right) => {
      const leftOrder = getChunkWaterfallOrder({
        id: left.id,
        kind: "task",
        task: left,
      });
      const rightOrder = getChunkWaterfallOrder({
        id: right.id,
        kind: "task",
        task: right,
      });
      return leftOrder - rightOrder;
    });
    processItems.push(buildToolActivityItem(entry.id, orderedTasks));
  }
  return {
    processItems,
    processSections: buildProcessSections(processItems),
  };
};

const buildTranscriptFinalItem = (
  turn: Pick<AssistantExecutionTurn, "finalAnswer" | "id" | "isStreaming">,
): TranscriptViewModel["finalItem"] => {
  const finalText = turn.finalAnswer;
  if (!finalText.trim()) {
    return null;
  }
  return {
    kind: "assistantText",
    id: `${turn.id}-answer-text`,
    phase: turn.isStreaming ? "streaming" : "final",
    text: finalText,
    waterfall: {
      schema: "waterfall.v1",
      section: "final",
      groupId: `turn:${turn.id}:final`,
      displayRole: turn.isStreaming
        ? "assistant_final_streaming"
        : "assistant_final",
      collapsePolicy: "never",
      order: 30,
    },
  };
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
  const operationInlineSummary = formatOperationInlineSummary(
    operation,
    statusClassName,
    durationText,
    metaText,
  );
  const leafSummary = operationInlineSummary;
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
    if (atom.pathOpenable && isOperationPathOpenable(operation) && path && onOpenWorkspacePath) {
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

const SubagentTranscriptTag = memo(function SubagentTranscriptTag({
  entry,
  onOpenAgentSession,
}: {
  entry: SubagentResult;
  onOpenAgentSession?: AgentResultStreamProps["onOpenAgentSession"];
}) {
  const subagent = useChatViewStore(
    useShallow((state) => state.subagentById[entry.id] ?? entry),
  );
  const title = subagent.description?.trim() || subagent.title;
  const canOpen = Boolean(subagent.childSessionId && onOpenAgentSession);
  return (
    <button
      type="button"
      className={`agentSubagentTag is-${subagent.status}`}
      disabled={!canOpen}
      onClick={() => {
        if (subagent.childSessionId) {
          onOpenAgentSession?.(subagent.childSessionId, title);
        }
      }}
    >
      <span className="agentSubagentTagMark" aria-hidden="true" />
      <span className="agentSubagentTagRole">Agent</span>
      <span className="agentSubagentTagTitle">{title}</span>
    </button>
  );
});

const TaskGroupTranscriptItem = memo(function TaskGroupTranscriptItem({
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

  const renderToolTaskBody = (defaultOperationOpen: boolean): ReactNode => {
    return (
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
  };

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
        key={entry.id}
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
      key={entry.id}
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

export function AgentResultStream({
  turn,
  onOpenAgentSession,
  onOpenWorkspacePath,
}: AgentResultStreamProps) {
  const processTranscript = useMemo(
    () => buildTranscriptProcessViewModel(turn),
    [turn.chunks],
  );
  const finalItem = useMemo(
    () => buildTranscriptFinalItem(turn),
    [turn.finalAnswer, turn.id, turn.isStreaming],
  );
  const hasFinalItem = finalItem?.kind === "assistantText";
  const subagents = useMemo(
    () => turn.chunks
      .filter((chunk): chunk is SubagentChunk => chunk.kind === "subagent")
      .map((chunk) => chunk.subagent),
    [turn.chunks],
  );
  const activity = turn.activity;
  const subagentIds = new Set(subagents.map((subagent) => subagent.subagentId));
  const liveSubagentIds = new Set(
    subagents
      .filter((subagent) => subagent.status === "running")
      .map((subagent) => subagent.subagentId),
  );
  const tachikomaCount = tachikomaEasterEgg(
    turn.agentRunId,
    subagentIds.size,
    liveSubagentIds.size,
  );
  const hasTachikoma = tachikomaCount !== null;
  const activityLabel = runtimeEasterEgg(
    turn.agentRunId,
    activity?.processState,
  ) ?? activity?.label ?? "";
  const hasRunningTool = turn.chunks.some(
    (chunk) => chunk.kind === "task" && chunk.task.status === "running",
  );

  const renderProcessHeading = (entry: TranscriptTextItem) => (
    <div
      className={`agentProcessSectionHeading ${entry.tone === "error" ? "is-error" : ""} ${entry.phase === "compaction" ? "is-compaction" : ""}`}
      data-waterfall-section={entry.waterfall?.section ?? "process"}
      key={entry.id}
    >
      {entry.phase === "compaction" ? entry.text : (
        <MarkdownContent
          text={entry.text}
          onOpenWorkspacePath={onOpenWorkspacePath}
        />
      )}
    </div>
  );

  const renderTranscriptItem = (entry: TranscriptItem) => {
    if (entry.kind === "guidedSupplement") {
      return (
        <div
          className="agentGuidedSupplement"
          data-waterfall-section="process"
          key={entry.id}
        >
          <div className="agentGuidedSupplementLabel">
            <CornerDownLeft
              className="agentGuidedSupplementIcon"
              aria-hidden="true"
            />
            <span>已引导对话</span>
          </div>
          <div className="agentGuidedSupplementBubble">
            <MarkdownContent
              text={entry.text}
              onOpenWorkspacePath={onOpenWorkspacePath}
            />
          </div>
        </div>
      );
    }
    if (entry.kind === "assistantText") {
      return renderProcessHeading(entry);
    }

    return (
      <TaskGroupTranscriptItem
        entry={entry}
        key={entry.id}
        onOpenWorkspacePath={onOpenWorkspacePath}
      />
    );
  };

  const processFeed = (
    <div className="agentProcessSections">
      {processTranscript.processSections.map((section) => (
        <section className="agentProcessSection" key={section.id}>
          {section.heading ? renderProcessHeading(section.heading) : null}
          {section.items.length > 0 ? (
            <div className="agent-inline-feed unified-feed agentProcessFeed">
              {section.items.map((entry) => renderTranscriptItem(entry))}
            </div>
          ) : null}
        </section>
      ))}
    </div>
  );
  const liveStatus = turn.isStreaming && (hasTachikoma || activity) &&
    (hasTachikoma || !hasRunningTool) && !hasFinalItem ? (
    <div className="agentStatusRow">
      <div className="agentRunStatus" aria-live="polite">
        <span className="agentRunStatusText">
          {hasTachikoma ? (
            <>
              Tachikoma{" "}
              <span className="tachikomaCount" key={tachikomaCount}>
                ×{tachikomaCount}
              </span>
              {tachikomaCount === 1 ? " · awaiting result…" : " · whispering…"}
            </>
          ) : activityLabel}
        </span>
      </div>
    </div>
  ) : null;
  return (
    <div className="agentResultBash">
      <div className="agentResultMain">
        {processTranscript.processItems.length > 0 || liveStatus ? (
          <div className="agentProcessLive">
            {processFeed}
            {liveStatus}
          </div>
        ) : null}

        {subagents.length > 0 ? (
          <div className="agentSubagentTags" aria-label="Agent 会话">
            {subagents.map((subagent) => (
              <SubagentTranscriptTag
                entry={subagent}
                key={subagent.id}
                onOpenAgentSession={onOpenAgentSession}
              />
            ))}
          </div>
        ) : null}

        {hasFinalItem ? (
          <div
            className="agentAssistantAnswer answerMarkdownBlock"
            data-waterfall-section={finalItem.waterfall?.section ?? "final"}
          >
            <div className="answer-content" key={finalItem.id}>
              <MarkdownContent
                text={finalItem.text}
                isStreaming={finalItem.phase === "streaming"}
                onOpenWorkspacePath={onOpenWorkspacePath}
              />
            </div>
          </div>
        ) : null}
      </div>
    </div>
  );
}
