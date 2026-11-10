const TOOL_ACTIVITY_DEFINITIONS = Object.freeze({
  bash: { kind: "command", title: "Ran commands", detailRendererKind: "bash", runningVerb: "Running", expandable: true },
  read: { kind: "read", title: "Read files", detailRendererKind: "file", runningVerb: "Reading", expandable: true },
  write: { kind: "edit", title: "Wrote files", detailRendererKind: "diff", runningVerb: "Writing", expandable: true },
  edit: { kind: "edit", title: "Edited files", detailRendererKind: "diff", runningVerb: "Editing", expandable: true },
  web_search: { kind: "webSearch", title: "Searched the web", detailRendererKind: "none", runningVerb: "Searching", expandable: false },
  agent: { kind: "agent", title: "Ran an agent", detailRendererKind: "none", runningVerb: "Running", expandable: false },
  task_output: { kind: "taskOutput", title: "Read task results", detailRendererKind: "none", runningVerb: "Reading", expandable: false },
  publish_artifact: { kind: "publishArtifact", title: "Published artifacts", detailRendererKind: "none", runningVerb: "Publishing", expandable: false },
});

function getToolActivityDefinition(toolName) {
  const definition = TOOL_ACTIVITY_DEFINITIONS[toolName];
  if (!definition) throw new Error(`unsupported tool activity: ${toolName || "<missing>"}`);
  return definition;
}

const TOOL_ICONS = {
  agent: "agent", command: "terminal", edit: "edit", publishArtifact: "fileOutput",
  read: "search", taskOutput: "listChecks", webSearch: "globe",
};

const identity = (text) => text;

function webToolAtom(atom, translate = identity) {
  return { ...atom, title: translate(atom.title), runningVerb: translate(atom.runningVerb), icon: TOOL_ICONS[atom.kind] || atom.icon, detail: atom.detailRendererKind };
}

export function toolAtom(toolName, translate = identity) {
  return webToolAtom(getToolActivityDefinition(toolName), translate);
}

export function activityToolAtom(toolName, providerId, translate = identity) {
  try {
    return toolAtom(toolName, translate);
  } catch (error) {
    if (!providerId || providerId === "centaeris.builtin" || !(error instanceof Error) || !error.message.startsWith("unsupported tool activity:")) throw error;
    return webToolAtom(TOOL_ACTIVITY_DEFINITIONS.bash, translate);
  }
}

export function activityTarget(activity) {
  const input = activity.call?.normalizedInput || {};
  return input.description || activity.call?.displayTarget || input.path || input.command || input.query || activity.toolName;
}

export function formatPhaseElapsed(startedAtMs, nowMs) {
  if (!Number.isFinite(startedAtMs) || !Number.isFinite(nowMs) || nowMs < startedAtMs) return "";
  const totalSeconds = Math.floor((nowMs - startedAtMs) / 1000);
  if (totalSeconds < 1) return "";
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  if (hours > 0) return `${hours}h ${String(minutes).padStart(2, "0")}m ${String(seconds).padStart(2, "0")}s`;
  if (minutes > 0) return `${minutes}m ${String(seconds).padStart(2, "0")}s`;
  return `${seconds}s`;
}

function bashDescription(activity) {
  if (activity.toolName !== "bash") return "";
  const input = activity.call?.normalizedInput || {};
  return typeof input.description === "string" ? input.description.trim() : "";
}

export function runningActivityPresentation(activity, translate = identity) {
  if (activity.status !== "running") throw new Error("live tool activity must be running");
  const atom = activityToolAtom(activity.toolName, activity.call?.providerId, translate);
  return {
    icon: atom.icon,
    label: bashDescription(activity) || (activity.toolName === "bash"
      ? translate("Running a command")
      : `${atom.runningVerb} ${activityTarget(activity)}`),
  };
}

export function toolActivityPresentation(activities, translate = identity) {
  const atoms = [];
  const kinds = new Set();
  for (const activity of activities) {
    const atom = activityToolAtom(activity.toolName, activity.call?.providerId, translate);
    if (!kinds.has(atom.kind)) {
      kinds.add(atom.kind);
      atoms.push(atom);
    }
  }
  if (!atoms.length) throw new Error("tool activity group is empty");
  const singleBash = activities.length === 1 && activities[0].toolName === "bash" ? activities[0] : null;
  return {
    atoms,
    title: singleBash
      ? bashDescription(singleBash) || translate(singleBash.status === "running" ? "Running a command" : "Ran a command")
      : atoms.map((atom) => atom.title).join(" · "),
    icon: atoms[0].icon,
    expandable: atoms.some((atom) => atom.expandable),
  };
}

export function groupActivities(activities, translate = identity) {
  if (!activities.length) return [];
  const turnIds = new Set(activities.map((activity) => activity.turnId));
  const status = activities.some((activity) => activity.status === "running")
    ? "running"
    : "completed";
  return [{
    turnId: turnIds.size === 1 ? activities[0].turnId : undefined,
    sequence: activities[0].sequence,
    activities,
    status,
    activityIds: activities.map((activity) => activity.activityId),
    presentation: toolActivityPresentation(activities, translate),
  }];
}

export function createAgentRunPresentationWork() {
  return {
    messageVisits: 0,
    activityVisits: 0,
    reasoningVisits: 0,
    sortComparisons: 0,
    displayEntryVisits: 0,
    processItemVisits: 0,
  };
}

export function buildAgentRunSections(messages, activities, reasoningBlocks = [], translate = identity, work) {
  if (work) {
    work.messageVisits += messages.length;
    work.activityVisits += activities.length;
    work.reasoningVisits += reasoningBlocks.length;
  }
  const displayEntries = [
    ...messages.filter((item) => item.role === "assistant").map((message) => ({ kind: "assistant", sequence: message.sequence, message })),
    ...activities.map((activity) => ({ kind: "tool", sequence: activity.sequence, activity })),
    ...reasoningBlocks.map((block) => ({ kind: "reasoning", sequence: block.sequence, block })),
  ].sort((left, right) => {
    if (work) work.sortComparisons += 1;
    return left.sequence - right.sequence;
  });
  const processItems = [];
  let pendingActivities = [];
  const flushActivities = () => {
    for (const group of groupActivities(pendingActivities, translate)) {
      processItems.push({ kind: "toolGroup", group });
    }
    pendingActivities = [];
  };
  for (const entry of displayEntries) {
    if (work) work.displayEntryVisits += 1;
    if (entry.kind === "tool") {
      pendingActivities.push(entry.activity);
      continue;
    }
    flushActivities();
    processItems.push(entry.kind === "reasoning"
      ? { kind: "reasoning", block: entry.block }
      : { kind: "assistant", message: entry.message });
  }
  flushActivities();

  const sections = [];
  let current = null;
  const flushSection = () => {
    if (current) sections.push(current);
    current = null;
  };
  for (const item of processItems) {
    if (work) work.processItemVisits += 1;
    if (item.kind === "assistant") {
      flushSection();
      current = {
        sectionId: item.message.messageId,
        turnId: item.message.turnId,
        sequence: item.message.sequence,
        message: item.message,
        items: [],
      };
      continue;
    }
    if (!current) {
      current = {
        sectionId: item.kind === "reasoning" ? `reasoning:${item.block.id}` : `tools:${item.group.activityIds[0]}`,
        turnId: item.kind === "reasoning" ? item.block.turnId : item.group.turnId,
        sequence: item.kind === "reasoning" ? item.block.sequence : item.group.sequence,
        message: null,
        items: [],
      };
    }
    current.items.push(item);
  }
  flushSection();
  return sections;
}

export function referenceCitations(citations) {
  const references = new Map();
  for (const citation of citations) {
    const existing = references.get(citation.inputRef);
    if (existing) existing.citations.push(citation);
    else references.set(citation.inputRef, { ...citation, citations: [citation] });
  }
  return [...references.values()];
}
