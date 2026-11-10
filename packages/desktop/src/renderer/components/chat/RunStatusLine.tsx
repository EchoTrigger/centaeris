import { memo, useEffect, useState } from "react";
import { Loader } from "lucide-react";
import { useTranslation } from "../../i18n";
import { useChatViewStore } from "./chatViewStore";
import { runtimeEasterEgg, tachikomaEasterEgg } from "./chatRuntimeCore";
import { formatActiveDuration, type ActiveTask } from "./runActivity";
import type { RuntimeProcessState } from "./types";

function TaskClock({ startedAtMs }: { startedAtMs: number }) {
  const [now, setNow] = useState(Date.now);
  useEffect(() => { const timer = setInterval(() => setNow(Date.now()), 1000); return () => clearInterval(timer); }, []);
  const text = formatActiveDuration(Math.max(now, Date.now()) - startedAtMs);
  return text ? <time className="runStatusElapsed" aria-hidden="true">{text}</time> : null;
}
const toolLabels: Record<string, string> = { read:"reading", bash:"executing", edit:"editing", write:"editing", web_search:"searching", agent:"agents", task_output:"reading" };
function labelKey(task: ActiveTask) { return task.phase ?? toolLabels[task.label] ?? "executing"; }

export const RunStatusLine = memo(function RunStatusLine({ turnId }: { turnId: string }) {
  const snapshot = useChatViewStore(state => state.runActivityByTurnId?.[turnId]);
  const { t } = useTranslation();
  if (!snapshot) return null;
  const { current, turn } = snapshot;
  const agents = turn.chunks.filter(chunk => chunk.kind === "subagent");
  const count = current.spinning ? tachikomaEasterEgg(turn.agentRunId,
    new Set(agents.map(chunk => chunk.subagent.subagentId)).size,
    new Set(agents.filter(chunk => chunk.subagent.status === "running").map(chunk => chunk.subagent.subagentId)).size) : null;
  const egg = current.spinning ? runtimeEasterEgg(turn.agentRunId, current.phase as RuntimeProcessState | undefined) : null;
  const label = count !== null ? `Tachikoma ×${count}${count === 1 ? " · awaiting result…" : " · whispering…"}`
    : egg ?? t(`runStatus.${labelKey(current)}`, { defaultValue: current.label });
  return <div className="runStatusLine">
    {current.spinning ? <Loader className="runStatusSpinner" aria-hidden="true" /> : null}
    <span role="status">{label}</span>
    <TaskClock key={current.id} startedAtMs={current.startedAtMs} />
  </div>;
});
