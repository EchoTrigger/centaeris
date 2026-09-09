import { t } from "../../i18n";
import { toPositiveInt } from "./numberUtils";
import { isRecord } from "./chatRuntimeCore";
import type { ToolOperation } from "./types";

const toInt = (value: unknown): number | undefined =>
  typeof value === "number" && Number.isInteger(value) ? value : undefined;

const TOOL_RESULT_STATES = new Set([
  "successNoOutput",
  "successWithOutput",
  "successNoMatches",
  "failed",
  "denied",
  "aborted",
]);

const normalizeRequiredString = (value: unknown, field: string): string => {
  const normalized = typeof value === "string" ? value.trim() : "";
  if (!normalized) {
    throw new Error(t("chatToolRuntimeModel.toolresultPayloadOperationsIsMissingValue", { value1: field }));
  }
  return normalized;
};

const normalizeOptionalString = (value: unknown): string | undefined => {
  const normalized = typeof value === "string" ? value.trim() : "";
  return normalized || undefined;
};

export const normalizeToolOperation = (raw: unknown): ToolOperation => {
  if (!isRecord(raw)) {
    throw new Error(t("chatToolRuntimeModel.toolresultPayloadOperationsMustBeAnObject"));
  }
  const callId = normalizeRequiredString(raw.callId, "callId");
  const toolName = normalizeToolName(raw.toolName);
  const status = normalizeRequiredString(raw.status, "status");
  const resultState = normalizeRequiredString(raw.resultState, "resultState");
  if (!TOOL_RESULT_STATES.has(resultState)) {
    throw new Error(t("chatToolRuntimeModel.unsupportedToolresultPayloadOperationsResultstateValue", { value1: resultState }));
  }
  if (Object.hasOwn(raw, "title")) {
    throw new Error(t("chatToolRuntimeModel.toolresultPayloadOperationsDoesNotSupportTheOldTitle"));
  }
  const kind = typeof raw.kind === "string" ? raw.kind.trim() : undefined;
  if (toolName === "bash") {
    if (kind !== "command") {
      throw new Error(t("chatToolRuntimeModel.unsupportedToolOperationKindValueValue", { value1: toolName, value2: kind || "<missing>" }));
    }
  } else if (kind !== undefined) {
    throw new Error(t("chatToolRuntimeModel.unsupportedToolOperationKindValueValue", { value1: toolName, value2: kind }));
  }
  return {
    callId,
    toolName,
    kind,
    status,
    resultState,
    path: typeof raw.path === "string" ? raw.path.trim() : undefined,
    startLine: toPositiveInt(raw.startLine),
    endLine: toPositiveInt(raw.endLine),
    totalLines: toPositiveInt(raw.totalLines),
    nextOffset: toPositiveInt(raw.nextOffset),
    truncatedBy:
      typeof raw.truncatedBy === "string"
        ? raw.truncatedBy.trim()
        : undefined,
    query: typeof raw.query === "string" ? raw.query.trim() : undefined,
    matchCount: toPositiveInt(raw.matchCount),
    added: toPositiveInt(raw.added),
    removed: toPositiveInt(raw.removed),
    lines: toPositiveInt(raw.lines),
    text: normalizeOptionalString(raw.text),
    outputPreview: normalizeOptionalString(raw.outputPreview),
    diffPreview: normalizeOptionalString(raw.diffPreview),
    error: normalizeOptionalString(raw.error),
    exitCode: toInt(raw.exitCode),
  };
};

export const parseToolOperations = (
  raw: unknown,
  expected: { callId: string; toolName: string },
): ToolOperation[] => {
  if (!Array.isArray(raw)) {
    throw new Error(t("chatToolRuntimeModel.toolresultPayloadOperationsMustBeAnArray"));
  }
  return raw.map((item) => {
    const operation = normalizeToolOperation(item);
    if (
      operation.callId !== expected.callId ||
      operation.toolName !== expected.toolName
    ) {
      throw new Error(t("chatToolRuntimeModel.toolresultPayloadOperationsIdentityMismatch"));
    }
    return operation;
  });
};

export const toDisplayPath = (path?: string): string => {
  const normalized = (path || "").trim();
  if (!normalized) {
    return "";
  }
  if (normalized.length <= 72) {
    return normalized;
  }
  return `...${normalized.slice(-69)}`;
};

export const toLineRange = (startLine?: number, endLine?: number): string => {
  if (!startLine) {
    return "";
  }
  if (!endLine || endLine === startLine) {
    return t("chatToolRuntimeModel.lineValue", { value1: startLine });
  }
  return t("chatToolRuntimeModel.linesValueToValue", { value1: startLine, value2: endLine });
};

export const normalizeToolName = (value: unknown): string => {
  if (
    typeof value !== "string" ||
    !/^[a-z][a-z0-9]*(?:_[a-z0-9]+)*$/.test(value)
  ) {
    throw new Error(t("chatToolRuntimeModel.toolnameMustUseCanonicalLowerSnakeCase"));
  }
  return value;
};
