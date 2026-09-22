import { getToolActivityDefinition } from "../../lib/toolPresentation";
import type { ChatMessage, TaskResult } from "./types";

export function taskFamily(task: TaskResult): string {
  const name = task.operations?.[0]?.toolName;
  return name ? getToolActivityDefinition(name).kind : task.title;
}

// Group display records, never reorder or mutate authoritative transcript blocks.
export function historyProcessEntries(messages: readonly ChatMessage[]): ChatMessage[] {
  const entries: ChatMessage[] = [];
  for (const message of messages) {
    if (message.role === "assistant" && !message.turn.finalAnswer && !message.turn.chunks.length && !message.transcriptText) continue;
    const previous = entries.at(-1);
    const chunk = message.role === "assistant" && !message.turn.finalAnswer && message.turn.chunks.length === 1
      ? message.turn.chunks[0] : undefined;
    const last = previous?.role === "assistant" ? previous.turn.chunks.at(-1) : undefined;
    if (chunk?.kind === "task" && previous?.role === "assistant" && !previous.turn.finalAnswer
      && previous.turn.chunks.every(item => item.kind === "task") && last?.kind === "task"
      && taskFamily(last.task) === taskFamily(chunk.task)) {
      entries[entries.length - 1] = { ...previous, turn: { ...previous.turn, chunks: [...previous.turn.chunks, chunk] } };
    } else entries.push(message);
  }
  return entries;
}

export function historyProcessTiming(messages: readonly ChatMessage[]) {
  const turns = messages.flatMap(message => message.role === "assistant" ? [message.turn] : []);
  const starts = turns.filter(turn => turn.projectionRunId && turn.startedAtMs !== undefined);
  if (starts.length !== 1) return {};
  const start = starts[0];
  const end = turns.find(turn => turn.projectionRunId === start.projectionRunId && turn.completedAtMs !== undefined);
  return end && end.completedAtMs! >= start.startedAtMs!
    ? { startedAtMs: start.startedAtMs, completedAtMs: end.completedAtMs } : {};
}
