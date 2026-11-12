import { memo } from "react";
import { CircleCheck, CircleDashed, CircleStop, CircleX, LoaderCircle } from "lucide-react";
import { useTranslation } from "../i18n";
import { sortSessionSubagents, type SessionSubagent } from "./sessionSubagents";
import "./session-subagents.css";

const gridColors = ["#86d6ae", "#b5d98a", "#e6d27e", "#e8ae78"];
const statusIcons = { running: LoaderCircle, queued: CircleDashed, leased: CircleDashed,
  succeeded: CircleCheck, failed: CircleX, dead_lettered: CircleX, cancelled: CircleStop };
export function AgentGridMark({ seed }: { seed: string }) {
  let hash = 2166136261;
  for (let index = 0; index < seed.length; index++) hash = Math.imul(hash ^ seed.charCodeAt(index), 16777619) >>> 0;
  const cells = [];
  for (let y = 0; y < 5; y++) for (let x = 0; x < 5; x++) {
    const column = Math.min(x, 4 - x);
    if ((hash >>> (y * 3 + column)) & 1 || (x === 2 && y === 2)) cells.push(<rect key={`${x}:${y}`} x={x} y={y} width="0.8" height="0.8" rx="0.12" />);
  }
  return <svg className="sessionSubagentMark" viewBox="0 0 5 5" fill="currentColor" style={{ color: gridColors[hash % gridColors.length] }} aria-hidden="true">{cells}</svg>;
}

export function SessionSubagentStatus({ agent }: { agent: SessionSubagent }) {
  const { t } = useTranslation();
  const Icon = statusIcons[agent.status];
  return <span className={`sessionSubagentStatus is-${agent.status}`}>
    <Icon className={agent.status === "running" ? "sessionSubagentSpinner" : undefined} size={13} aria-hidden="true" />
    <span>{t(`sessionSubagent.status.${agent.status}`)}</span>
  </span>;
}

export const SessionSubagentTags = memo(function SessionSubagentTags({ agents, onOpen }: {
  agents: readonly SessionSubagent[];
  onOpen(agent: SessionSubagent, origin: { elementId: string }): void;
}) {
  const { t } = useTranslation();
  return <div className="sessionSubagentTags" aria-label={t("sessionSubagent.conversations")}>
    {sortSessionSubagents(agents).map(agent => <button type="button" className="sessionSubagentTag" key={agent.runtimeJobId}
      id={`subagent-${agent.runtimeJobId}`} aria-label={`Agent ${agent.description}: ${t(`sessionSubagent.status.${agent.status}`)}`}
      onClick={event => onOpen(agent, { elementId: event.currentTarget.id })}>
      <AgentGridMark seed={agent.childSessionId} />
      <span className="sessionSubagentRole">Agent</span>
      <span className="sessionSubagentTitle">{agent.description}</span>
      <SessionSubagentStatus agent={agent} />
    </button>)}
  </div>;
});
