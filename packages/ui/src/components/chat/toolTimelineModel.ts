import type { TaskResult, ToolOperation } from "./types";

export type ToolTimelineOperation = ToolOperation & {
  normalizedInput?: Record<string, unknown>;
  displayTarget?: string;
};

export type ToolTimelineTask = Pick<TaskResult, "operations">;

const compactText = (value: unknown, maxLength: number = 120): string => {
  const text = String(value ?? "").replace(/\s+/g, " ").trim();
  if (text.length <= maxLength) {
    return text;
  }
  return `${text.slice(0, Math.max(0, maxLength - 1)).trimEnd()}...`;
};

const inputString = (
  operation: ToolTimelineOperation,
  key: string,
): string | undefined => {
  const value = operation.normalizedInput?.[key];
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
};

export const getOperationCommand = (
  operation: ToolTimelineOperation,
): string | undefined => inputString(operation, "command");

export const getOperationPath = (
  operation: ToolTimelineOperation,
): string | undefined => operation.path?.trim() || inputString(operation, "path");

export const getOperationQuery = (
  operation: ToolTimelineOperation,
): string | undefined => operation.query?.trim() || inputString(operation, "query");

export const isCommandOperation = (
  operation: ToolTimelineOperation,
): boolean => operation.toolName === "bash";

export const getDisplayOperationsForTask = (
  task: ToolTimelineTask,
): ToolOperation[] => task.operations || [];

export const isBashCommandOperation = isCommandOperation;

export const formatOperationLineCoverage = (
  operation: ToolTimelineOperation,
): string | undefined => {
  if (operation.startLine === undefined) {
    return undefined;
  }
  const endLine = operation.endLine ?? operation.startLine;
  return `lines ${operation.startLine}-${endLine}${operation.totalLines ? ` of ${operation.totalLines}` : ""}`;
};

export const isOperationPathOpenable = (
  operation: ToolTimelineOperation,
): boolean => {
  if (!getOperationPath(operation)) {
    return false;
  }
  return ["read", "write", "edit"].includes(operation.toolName);
};

export const formatCompactCommandLine = (
  command: string,
): string => `$ ${compactText(command, 180)}`;

export const formatFullCommandLine = (
  command: string,
): string => `$ ${command.trim()}`;
