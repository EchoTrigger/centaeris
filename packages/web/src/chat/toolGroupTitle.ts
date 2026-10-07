export type ToolGroupOperationStatus = "running" | "completed" | "error";

export type ToolGroupOperation = Readonly<{
  toolName: string;
  description: string;
  target: string;
  status: ToolGroupOperationStatus;
}>;

export type ToolGroupCategory =
  | "edit"
  | "publishArtifact"
  | "command"
  | "read"
  | "webSearch"
  | "agent"
  | "taskOutput";

type Translate = (key: string, options?: Record<string, unknown>) => string;

const TOOL_CATEGORIES: Record<string, ToolGroupCategory> = {
  bash: "command",
  read: "read",
  write: "edit",
  edit: "edit",
  publish_artifact: "publishArtifact",
  web_search: "webSearch",
  agent: "agent",
  task_output: "taskOutput",
};

// Categories merge into a single clause and always render in this order, not in
// call order. Anything unrecognized is treated as a command (CLI or MCP tool).
export const TOOL_GROUP_CATEGORY_ORDER: readonly ToolGroupCategory[] = [
  "edit",
  "publishArtifact",
  "command",
  "read",
  "webSearch",
  "agent",
  "taskOutput",
];

function toolCategory(toolName: string): ToolGroupCategory {
  return TOOL_CATEGORIES[toolName] ?? "command";
}

// The group header icon is the first category present in display order.
export function toolGroupIconCategory(
  operations: readonly ToolGroupOperation[],
): ToolGroupCategory | null {
  const present = new Set(operations.map((operation) => toolCategory(operation.toolName)));
  return TOOL_GROUP_CATEGORY_ORDER.find((category) => present.has(category)) ?? null;
}

// Success and failure share one form; a running tool uses its progressive verb.
function verbStatus(status: ToolGroupOperationStatus): string {
  return status === "running" ? "running" : "completed";
}

// Mirrors the desktop tool card's grouped title copy, localized. Categories are
// merged, ordered by TOOL_GROUP_CATEGORY_ORDER, and counts always use digits.
export function formatToolGroupTitle(
  operations: readonly ToolGroupOperation[],
  t: Translate,
): string {
  if (operations.length === 0) return t("toolGroup.unknown");
  if (operations.length === 1) {
    const operation = operations[0];
    const category = toolCategory(operation.toolName);
    if (category === "command" && operation.description) return operation.description;
    if ((category === "read" || category === "edit") && operation.target) {
      return `${t(`toolGroup.${category}.${verbStatus(operation.status)}`)} ${operation.target}`;
    }
    if (category === "webSearch") {
      return t(`toolGroup.webSearch.${verbStatus(operation.status)}`);
    }
  }
  const groups = new Map<ToolGroupCategory, { count: number; status: ToolGroupOperationStatus }>();
  for (const operation of operations) {
    const category = toolCategory(operation.toolName);
    const existing = groups.get(category);
    if (existing === undefined) {
      groups.set(category, { count: 1, status: operation.status });
      continue;
    }
    existing.count += 1;
    if (operation.status === "running") existing.status = operation.status;
  }
  return TOOL_GROUP_CATEGORY_ORDER
    .filter((category) => groups.has(category))
    .map((category) => {
      const group = groups.get(category)!;
      if (category === "webSearch") {
        return t(`toolGroup.webSearch.${verbStatus(group.status)}`);
      }
      const verb = t(`toolGroup.${category}.${verbStatus(group.status)}`);
      const noun = t(`toolGroup.${category}.${group.count === 1 ? "one" : "other"}`);
      return `${verb} ${group.count} ${noun}`;
    })
    .join(t("toolGroup.join"));
}
