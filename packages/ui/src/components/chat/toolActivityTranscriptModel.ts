import { t } from "../../i18n";
import { formatDuration } from "./agentDuration";
import {
  compactText,
  taskStatusLabel,
  toDisplayPath,
} from "./chatRuntimeModel";
import {
  formatOperationLineCoverage,
  getDisplayOperationsForTask,
  getOperationCommand,
  getOperationPath,
  getOperationQuery,
  isCommandOperation,
} from "./toolTimelineModel";
import {
  getToolActivityAtom,
  type ToolActivityAtom,
} from "./toolActivityModel";
import type {
  TaskResult,
  TaskStatus,
  TimelineOperation,
} from "./types";

export type OperationDetailState = {
  command: string | undefined;
  path: string | undefined;
  showBashCommandInput: boolean;
  hasBashDetail: boolean;
  hasEditDetail: boolean;
  hasTextDetail: boolean;
};

export const collectTimelineOperations = (
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
        transcriptContentRef: task.transcriptContentRef,
        transcriptSessionId: task.transcriptSessionId,
        transcriptProjectionGeneration: task.transcriptProjectionGeneration,
      });
    }
  }
  return operations;
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

export const formatToolGroupTitle = (
  operations: TimelineOperation[],
): string => {
  if (operations.length === 1) {
    const operation = operations[0];
    const atom = getToolActivityAtom(operation);
    const status = operationStatusClass(operation.status);
    const target = operationPrimaryTarget(operation);
    if (target) return `${operationInlineVerb(operation, status)} ${target}`;
    if (atom.kind === "webSearch") {
      return formatToolCountTitle(operation, 1, status);
    }
  }
  const groups: Array<{
    operation: TimelineOperation;
    count: number;
    status: TaskStatus;
  }> = [];
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
    .map(({ operation, count, status }) =>
      formatToolCountTitle(operation, count, status)
    )
    .join(", ");
};

export const formatCompletedToolGroupTitle = (
  tasks: TaskResult[],
): string => formatToolGroupTitle(collectTimelineOperations(tasks));

export const formatRunningToolGroupTitle = (tasks: TaskResult[]): string => {
  const operations = collectTimelineOperations(tasks);
  if (operations.length === 0) {
    return compactText(tasks[tasks.length - 1]?.title || "Thinking", 180);
  }
  return formatToolGroupTitle(operations);
};

export const operationStatusClass = (status?: string): TaskStatus => {
  const normalized = (status || "").trim().toLowerCase();
  if (["error", "failed", "timeout", "blocked"].includes(normalized)) {
    return "error";
  }
  if (["running", "pending", "started"].includes(normalized)) {
    return "running";
  }
  return "done";
};

export const formatTimelineMeta = (
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

export const operationBashStatusLabel = (
  operation: TimelineOperation,
  statusClassName: TaskStatus,
): string => statusClassName === "done"
  ? "Succeeded"
  : operationStatusLabel(operation, statusClassName);

export const formatOperationDuration = (
  operation: TimelineOperation,
): string | undefined => typeof operation.durationMs === "number"
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

// Actual tool input takes priority over descriptions and generic result labels.
const operationPrimaryTarget = (operation: TimelineOperation): string | undefined => [
  isCommandOperation(operation) ? getOperationCommand(operation) : undefined,
  toDisplayPath(getOperationPath(operation)),
  getOperationQuery(operation),
  operation.displayTarget,
  operation.text === operation.toolName ? undefined : operation.text,
].map(value => String(value ?? "").trim()).find(Boolean);

export const formatOperationInlineSummary = (
  operation: TimelineOperation,
  statusClassName: TaskStatus,
  durationText?: string,
  metaText?: string,
): string => {
  const target = operationPrimaryTarget(operation);
  const parts = [
    target || operationInlineVerb(operation, statusClassName),
  ];
  if (metaText && !target?.includes(metaText)) {
    parts.push(metaText);
  }
  if (durationText) {
    parts.push(t("toolActivityTranscriptModel.elapsedValue", { value1: durationText }));
  }
  return parts.join("，");
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

export const getOperationDetailState = (
  operation: TimelineOperation,
  atom: ToolActivityAtom,
  statusClassName: TaskStatus,
): OperationDetailState => {
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
      operation.transcriptContentRef ||
      operation.error,
    );
  const hasEditDetail =
    atom.detailRendererKind === "diff" &&
    Boolean(operation.diffPreview || operation.outputPreview || operation.error);
  const hasTextDetail =
    atom.detailRendererKind !== "bash" &&
    Boolean(
      operation.fullOutputPath ||
      operation.transcriptContentRef ||
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

// Byte boundaries are supplied by Core for known result formats, never inferred from prose.
export function readableToolOutput(operation: Pick<TimelineOperation, "contentStartByte" | "contentByteLength">, text: string, offset = 0, raw = false): string {
  const start = operation.contentStartByte;
  const length = operation.contentByteLength;
  if (raw || start === undefined || length === undefined) return text;
  if (!Number.isSafeInteger(start) || !Number.isSafeInteger(length) || start < 0 || length < 0) throw new Error("Invalid readable output range");
  const bytes = new TextEncoder().encode(text);
  const from = Math.max(0, start - offset);
  const to = Math.min(bytes.length, start + length - offset);
  return to <= from ? "" : new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(from, to));
}
