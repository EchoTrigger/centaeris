import type { AgentRunSummary } from "../../lib/chatBridge";
import type { UiSession } from "../../types/ui";
import { isActiveAgentRun, isTerminalAgentRunStatus, normalizeAgentRunStatus } from "../chat/chatRuntimeModel";

type ActivityPorts = {
  listRuns: () => Promise<AgentRunSummary[]>;
  readQuestionCount: (sessionId: string) => Promise<number>;
};
export type SessionActivity = {
  runningSessionIds: Set<string>;
  completedSessionIds: Set<string>;
  terminalSessionIds: Set<string>;
  waitingSessionIds: Set<string>;
  checkedWaitingSessionIds: Set<string>;
};

// One discovery owner; existing Runtime facts supply state for detached chats.
// Only main chats in the visible catalog are observed, with two host reads at a time.
export async function readSessionActivity(
  sessions: readonly Pick<UiSession, "id" | "sessionKind">[],
  selectedSessionId: string | null,
  ports: ActivityPorts,
  isCurrent: () => boolean = () => true,
  observedSessionIds: ReadonlySet<string> = new Set(),
): Promise<SessionActivity | null> {
  const runs = await ports.listRuns();
  if (!isCurrent()) return null;
  const visible = new Set(sessions.filter(session => session.sessionKind === "main").map(session => session.id));
  const activity: SessionActivity = {
    runningSessionIds: new Set(), completedSessionIds: new Set(), terminalSessionIds: new Set(),
    waitingSessionIds: new Set(), checkedWaitingSessionIds: new Set(),
  };
  for (const run of runs) {
    if (!visible.has(run.sessionId)) continue;
    if (isActiveAgentRun(run)) activity.runningSessionIds.add(run.sessionId);
    if (isTerminalAgentRunStatus(normalizeAgentRunStatus(run.status))) {
      activity.terminalSessionIds.add(run.sessionId);
      if (run.unread && run.sessionId !== selectedSessionId) activity.completedSessionIds.add(run.sessionId);
    }
  }
  // A finished run can still own an unanswered checkpoint.
  const candidates = new Set([...activity.runningSessionIds, ...activity.completedSessionIds, ...observedSessionIds]);
  const background = [...candidates].filter(id => visible.has(id));
  for (let offset = 0; offset < background.length; offset += 2) {
    if (!isCurrent()) return null;
    const ids = background.slice(offset, offset + 2);
    const results = await Promise.allSettled(ids.map(id => ports.readQuestionCount(id)));
    if (!isCurrent()) return null;
    results.forEach((result, index) => {
      if (result.status !== "fulfilled") return;
      activity.checkedWaitingSessionIds.add(ids[index]);
      if (result.value > 0) activity.waitingSessionIds.add(ids[index]);
    });
  }
  return activity;
}

export function retainEqualSet(previous: Set<string>, next: Set<string>): Set<string> {
  return previous.size === next.size && [...next].every(id => previous.has(id)) ? previous : next;
}
