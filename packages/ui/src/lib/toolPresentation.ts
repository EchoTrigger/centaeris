export type ToolActivityKind =
  | "edit"
  | "read"
  | "command"
  | "webSearch"
  | "agent"
  | "taskOutput"
  | "externalTool";

export type ToolDetailRendererKind = "bash" | "diff" | "file" | "none";

export type ToolActivityDefinition = Readonly<{
  kind: ToolActivityKind;
  title: string;
  detailRendererKind: ToolDetailRendererKind;
  runningVerb: string;
  completedVerb: string;
  failedVerb: string;
  pathOpenable: boolean;
  expandable: boolean;
}>;

const definitions: Record<string, ToolActivityDefinition> = {
  bash: { kind: "command", title: "Ran commands", detailRendererKind: "bash", runningVerb: "Running", completedVerb: "Ran", failedVerb: "Run failed", pathOpenable: false, expandable: true },
  read: { kind: "read", title: "Read files", detailRendererKind: "file", runningVerb: "Reading", completedVerb: "Read", failedVerb: "Read failed", pathOpenable: true, expandable: true },
  write: { kind: "edit", title: "Wrote files", detailRendererKind: "diff", runningVerb: "Writing", completedVerb: "Wrote", failedVerb: "Write failed", pathOpenable: true, expandable: true },
  edit: { kind: "edit", title: "Edited files", detailRendererKind: "diff", runningVerb: "Editing", completedVerb: "Edited", failedVerb: "Edit failed", pathOpenable: true, expandable: true },
  web_search: { kind: "webSearch", title: "Searched the web", detailRendererKind: "none", runningVerb: "Searching", completedVerb: "Searched", failedVerb: "Search failed", pathOpenable: false, expandable: false },
  agent: { kind: "agent", title: "Ran an agent", detailRendererKind: "none", runningVerb: "Running", completedVerb: "Ran", failedVerb: "Run failed", pathOpenable: false, expandable: false },
  task_output: { kind: "taskOutput", title: "Read task results", detailRendererKind: "none", runningVerb: "Reading", completedVerb: "Read", failedVerb: "Read failed", pathOpenable: false, expandable: false },
};

const externalToolDefinition: ToolActivityDefinition = {
  kind: "externalTool",
  title: "Ran external tools",
  detailRendererKind: "none",
  runningVerb: "Running",
  completedVerb: "Ran",
  failedVerb: "Run failed",
  pathOpenable: false,
  expandable: false,
};

export function getToolActivityDefinition(toolName: string): ToolActivityDefinition {
  const definition = definitions[toolName];
  if (definition) return definition;
  if (!/^[a-z][a-z0-9]*(?:_[a-z0-9]+)*$/.test(toolName)) {
    throw new Error(`invalid tool activity: ${toolName || "<missing>"}`);
  }
  return externalToolDefinition;
}

export function getToolActivitySummary(toolNames: string[]) {
  if (!toolNames.length) throw new Error("tool activity group is empty");
  const unique = [...new Map(toolNames.map((name) => {
    const definition = getToolActivityDefinition(name);
    return [definition.kind, definition] as const;
  })).values()];
  return {
    definitions: unique,
    title: unique.map((definition) => definition.title).join(" · "),
    kind: unique[0].kind,
    expandable: unique.some((definition) => definition.expandable),
  };
}
