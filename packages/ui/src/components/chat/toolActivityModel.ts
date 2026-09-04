import {
  getToolActivityDefinition,
  getToolActivitySummary,
  type ToolActivityDefinition,
  type ToolActivityKind,
  type ToolDetailRendererKind,
} from "../../lib/toolPresentation";
import type { ToolOperation } from "./types";

export type { ToolActivityKind, ToolDetailRendererKind };

export type ToolActivityIconToken = ToolActivityKind;

export type ToolActivityAtom = ToolActivityDefinition & {
  iconToken: ToolActivityIconToken;
};

export type ToolActivityPresentation = {
  atoms: ToolActivityAtom[];
  title: string;
  iconToken: ToolActivityIconToken;
  expandable: boolean;
};

const isFailedResult = (resultState?: string): boolean =>
  ["failed", "denied", "aborted"].includes(resultState || "");

export const getToolActivityAtom = (
  operation: ToolOperation,
): ToolActivityAtom => {
  let atomDefinition: ToolActivityDefinition;
  try {
    atomDefinition = getToolActivityDefinition(operation.toolName);
  } catch {
    throw new Error(`不支持的工具 operation: ${operation.toolName || "<missing>"}`);
  }
  if (operation.toolName === "bash") {
    if (operation.resultState && operation.kind !== "command") {
      throw new Error(`工具 operation kind 不支持: bash/${operation.kind || "<missing>"}`);
    }
  } else if (operation.kind !== undefined) {
    throw new Error(`工具 operation kind 不支持: ${operation.toolName}/${operation.kind}`);
  }
  if (["write", "edit"].includes(operation.toolName) && operation.resultState) {
    if (isFailedResult(operation.resultState) && operation.diffPreview) {
      throw new Error(`失败 ${operation.toolName} operation 不得携带 diffPreview`);
    }
    if (!isFailedResult(operation.resultState) && !operation.diffPreview) {
      throw new Error(`成功 ${operation.toolName} operation 缺少 diffPreview`);
    }
  }
  return { ...atomDefinition, iconToken: atomDefinition.kind };
};

export const getToolActivityPresentation = (
  operations: ToolOperation[],
): ToolActivityPresentation => {
  for (const operation of operations) {
    getToolActivityAtom(operation);
  }
  const summary = getToolActivitySummary(operations.map((operation) => operation.toolName));
  const atoms = summary.definitions.map((atom) => ({ ...atom, iconToken: atom.kind }));
  return {
    atoms,
    title: summary.title,
    iconToken: summary.kind,
    expandable: summary.expandable,
  };
};
