import {
  getChunkWaterfallOrder,
  getChunkWaterfallSection,
} from "./chatRuntimeModel";
import { taskFamily } from "./historyProcessEntries";
import type {
  AssistantExecutionTurn,
  AgentDisplayEntry,
  TaskResult,
  TranscriptItem,
  TranscriptProcessSection,
  TranscriptToolGroupItem,
  TranscriptViewModel,
} from "./types";

export type TranscriptProjectionWork = {
  waterfallFilterVisits: number;
  inputChunkVisits: number;
  displayEntryVisits: number;
  sectionItemVisits: number;
  taskSortComparisons: number;
};

export const createTranscriptProjectionWork = (): TranscriptProjectionWork => ({
  waterfallFilterVisits: 0,
  inputChunkVisits: 0,
  displayEntryVisits: 0,
  sectionItemVisits: 0,
  taskSortComparisons: 0,
});

const buildAgentDisplayEntries = (
  chunks: AssistantExecutionTurn["chunks"],
  work?: TranscriptProjectionWork,
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
    if (work) work.inputChunkVisits += 1;
    if (chunk.kind === "task") {
      if (pendingTasks.length && taskFamily(pendingTasks[0]) !== taskFamily(chunk.task)) flushTasks();
      pendingTasks.push(chunk.task);
      continue;
    }
    flushTasks();
    if (chunk.kind === "reasoning") {
      entries.push({ kind: "reasoning", chunk });
      continue;
    }
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
  work?: TranscriptProjectionWork,
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
    if (work) work.sectionItemVisits += 1;
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
  work?: TranscriptProjectionWork,
): Pick<TranscriptViewModel, "processItems" | "processSections"> => {
  const waterfallChunks = turn.chunks.filter((chunk) => {
    if (work) work.waterfallFilterVisits += 1;
    return getChunkWaterfallSection(chunk) !== "final";
  });
  const processItems: TranscriptItem[] = [];
  for (const entry of buildAgentDisplayEntries(waterfallChunks, work)) {
    if (work) work.displayEntryVisits += 1;
    if (entry.kind === "reasoning") {
      processItems.push(entry.chunk);
      continue;
    }
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
      if (work) work.taskSortComparisons += 1;
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
    processSections: buildProcessSections(processItems, work),
  };
};

export const buildTranscriptFinalItem = (
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
