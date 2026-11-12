import { memo, useMemo } from "react";
import { CircleCheck, CircleDashed, CircleHelp, CirclePause, CircleStop, CircleX, LoaderCircle } from "lucide-react";
import { t } from "../../i18n";
import { sortSessionSubagents } from "./sessionSubagents";
import type { AgentResultStreamProps, SubagentResult, SubagentStatus } from "./types";

const gridColors = ["#86d6ae", "#b5d98a", "#e6d27e", "#e8ae78"];
const statusIcons = { running: LoaderCircle, queued: CircleDashed, waiting: CirclePause,
  done: CircleCheck, error: CircleX, cancelled: CircleStop, interrupted: CircleStop, unknown: CircleHelp };

function AgentGridMark({ seed }: { seed: string }) {
  let hash = 2166136261;
  for (let index = 0; index < seed.length; index++) hash = Math.imul(hash ^ seed.charCodeAt(index), 16777619) >>> 0;
  const cells = [];
  for (let y = 0; y < 5; y++) for (let x = 0; x < 5; x++) {
    const column = Math.min(x, 4 - x);
    if ((hash >>> (y * 3 + column)) & 1 || (x === 2 && y === 2)) cells.push(<rect key={`${x}:${y}`} x={x} y={y} width="0.8" height="0.8" rx="0.12" />);
  }
  return <svg className="agentSubagentTagMark" viewBox="0 0 5 5" fill="currentColor" style={{ color: gridColors[hash % gridColors.length] }} aria-hidden="true">{cells}</svg>;
}

const AgentSessionTag = memo(function AgentSessionTag({ entry, onOpenAgentSession }: {
  entry: SubagentResult;
  onOpenAgentSession: AgentResultStreamProps["onOpenAgentSession"];
}) {
  const title = entry.description?.trim() || entry.title;
  const status: SubagentStatus = entry.status;
  const StatusIcon = statusIcons[status];
  const statusText = t(`agentSession.status.${status}`);
  return <button type="button" className={`agentSubagentTag is-${status}`}
    disabled={!entry.childSessionId || !onOpenAgentSession}
    aria-label={`Agent ${title}: ${statusText}`} title={`${title}: ${statusText}`}
    onClick={() => { if (entry.childSessionId) onOpenAgentSession?.(entry.childSessionId, title); }}>
    <AgentGridMark seed={entry.childSessionId ?? entry.avatarSeed ?? entry.subagentId} />
    <span className="agentSubagentTagRole">Agent</span>
    <span className="agentSubagentTagTitle">{title}</span>
    <span className="agentSubagentStatus"><StatusIcon className={status === "running" ? "agentSubagentSpinner" : undefined} size={13} aria-hidden="true" /><span>{statusText}</span></span>
  </button>;
});

export const AgentSessionTags = memo(function AgentSessionTags({ subagents, onOpenAgentSession }: {
  subagents: SubagentResult[];
  onOpenAgentSession: AgentResultStreamProps["onOpenAgentSession"];
}) {
  const sorted = useMemo(() => sortSessionSubagents(subagents), [subagents]);
  if (!sorted.length) return null;
  return <div className="agentSubagentTags" aria-label={t("agentTranscriptSections.agentConversation")}>
    {sorted.map(entry => <AgentSessionTag key={entry.childSessionId ?? entry.subagentId} entry={entry} onOpenAgentSession={onOpenAgentSession} />)}
  </div>;
});
