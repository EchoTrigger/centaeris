import type { AgentRunSummary, SessionItem } from "../../lib/chatBridge";
import type { SubagentResult, SubagentStatus } from "./types";

export function subagentRunStatus(status: string): SubagentStatus {
  switch (status) {
    case "queued": return "queued";
    case "running": return "running";
    case "stalled": return "waiting";
    case "succeeded": return "done";
    case "failed": return "error";
    case "cancelled": return "cancelled";
    case "stopped": return "interrupted";
    default: throw new Error(`Unsupported subagent Run status: ${status}`);
  }
}

export function restoreSessionSubagents(sessionId: string, sessions: readonly SessionItem[], runs: readonly AgentRunSummary[]): SubagentResult[] {
  const children = new Map<string, SessionItem[]>();
  for (const session of sessions) {
    if (session.sessionKind !== "subagent" || !session.parentSessionId) continue;
    const siblings = children.get(session.parentSessionId) ?? [];
    siblings.push(session);
    children.set(session.parentSessionId, siblings);
  }
  const latestRuns = new Map<string, AgentRunSummary>();
  for (const run of runs) {
    const previous = latestRuns.get(run.sessionId);
    if (!previous || run.updatedAtMs > previous.updatedAtMs) latestRuns.set(run.sessionId, run);
  }
  const entries: SubagentResult[] = [];
  const visited = new Set([sessionId]);
  const parents = [sessionId];
  for (let index = 0; index < parents.length; index++) {
    for (const session of children.get(parents[index]) ?? []) {
      if (visited.has(session.id)) continue;
      visited.add(session.id);
      parents.push(session.id);
      const run = latestRuns.get(session.id);
      entries.push({
        id: session.id, subagentId: session.runtimeJobId ?? session.id,
        sessionId: session.parentSessionId, childSessionId: session.id, avatarSeed: session.id,
        title: session.title, description: session.title, summary: session.lastMessage ?? "",
        status: run ? subagentRunStatus(run.status) : "unknown",
        startedAtMs: run?.startedAtMs, updatedAtMs: run?.updatedAtMs ?? session.updatedAt,
        completedAtMs: run?.completedAtMs ?? undefined,
      });
    }
  }
  return entries;
}

const identity = (entry: SubagentResult) => entry.childSessionId ?? entry.subagentId;
const updatedAt = (entry: SubagentResult) => entry.updatedAtMs ?? entry.completedAtMs ?? entry.startedAtMs ?? 0;

export function mergeSessionSubagents(persisted: readonly SubagentResult[], live: readonly SubagentResult[]): SubagentResult[] {
  const entries = new Map(persisted.map(entry => [identity(entry), entry]));
  for (const entry of live) {
    const previous = entries.get(identity(entry));
    if (!previous || updatedAt(entry) > updatedAt(previous)) entries.set(identity(entry), entry);
  }
  return [...entries.values()];
}

export function sortSessionSubagents(entries: readonly SubagentResult[]): SubagentResult[] {
  return [...entries].sort((a, b) => Number(b.status === "running") - Number(a.status === "running")
    || updatedAt(b) - updatedAt(a) || identity(a).localeCompare(identity(b)));
}
