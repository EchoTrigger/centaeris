import type { AssistantExecutionTurn } from "./types";

export type ActiveTask = { id: string; startedAtMs: number; label: string; spinning: boolean; phase?: string };
export type RunActivity = { turn: AssistantExecutionTurn; tasks: ActiveTask[]; current: ActiveTask };
const stopped = new Set(["waiting", "auth_failed", "provider_unavailable", "provider_interrupted"]);

// View-only clocks are observed at ingestion, never copied into transcript or
// session snapshots. Row remounts therefore cannot restart a running task.
export function observeRunActivity(previous: RunActivity | undefined, turn: AssistantExecutionTurn, now: number): RunActivity | undefined {
  if (!turn.isStreaming) return undefined;
  if (previous?.turn === turn) return previous;
  const candidates: Omit<ActiveTask, "startedAtMs">[] = [];
  for (const chunk of turn.chunks) {
    if (chunk.kind === "task" && chunk.task.status === "running") {
      candidates.push({ id: `tool:${chunk.task.id}`, label: chunk.task.operations?.[0]?.toolName ?? chunk.task.title, spinning: true });
    } else if (chunk.kind === "subagent" && chunk.subagent.status === "running") {
      candidates.push({ id: `agent:${chunk.subagent.id}`, label: "agent", spinning: true });
    } else if (chunk.kind === "reasoning" && chunk.status === "streaming") {
      candidates.push({ id: `reason:${chunk.id}`, label: "thinking", phase: "thinking", spinning: true });
    }
  }
  const phase = turn.activity?.processState ?? turn.activity?.kind ?? (turn.finalAnswer ? "outputting" : "thinking");
  const blocked = stopped.has(phase) || turn.activity?.kind === "waiting";
  if (!candidates.length || blocked) {
    candidates.push({ id: `phase:${phase}`, phase, label: turn.activity?.label ?? phase, spinning: !blocked });
  }
  const tasks = candidates.map(candidate => ({ ...candidate,
    startedAtMs: previous?.tasks.find(task => task.id === candidate.id)?.startedAtMs ?? now,
  }));
  const current = blocked ? tasks[tasks.length - 1] : tasks.reduce((latest, task) => task.startedAtMs >= latest.startedAtMs ? task : latest);
  return { turn, tasks, current };
}

export function formatActiveDuration(elapsedMs: number): string {
  const seconds = Math.max(0, Math.floor(elapsedMs / 1000));
  return [[Math.floor(seconds / 3600), "h"], [Math.floor(seconds / 60) % 60, "m"], [seconds % 60, "s"]]
    .filter(([value]) => Number(value) > 0).map(([value, unit]) => `${value}${unit}`).join(" ");
}
