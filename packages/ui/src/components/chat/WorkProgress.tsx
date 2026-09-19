import { useEffect, useState, type ReactNode } from "react";
import { ChevronRight } from "lucide-react";
import { useTranslation } from "../../i18n";
import { formatWorkDuration } from "./workDuration";

export function WorkProgress({ running, finalStarted, startedAtMs, completedAtMs, children, response, responseIsProcess = false }: {
  running: boolean;
  finalStarted: boolean;
  startedAtMs?: number;
  completedAtMs?: number;
  children?: ReactNode;
  response?: ReactNode;
  responseIsProcess?: boolean;
}) {
  const { t } = useTranslation();
  const [now, setNow] = useState(Date.now);
  const [disclosure, setDisclosure] = useState({ finalStarted, expanded: !finalStarted });
  // A draft answer can become a process summary when a tool starts. Reopen on
  // that boundary, but allow manual disclosure until the next phase transition.
  if (disclosure.finalStarted !== finalStarted) {
    setDisclosure({ finalStarted, expanded: !finalStarted });
  }
  useEffect(() => {
    if (!running) return;
    const interval = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(interval);
  }, [running]);
  const elapsed = startedAtMs === undefined ? "" : formatWorkDuration(
    (completedAtMs ?? now) - startedAtMs, t,
  );
  const label = [t(running ? "workProgress.working" : "workProgress.worked"), elapsed].filter(Boolean).join(" ");
  return <div className="workProgress">
    {finalStarted ? <button className="workProgressSummary" type="button" aria-label={label}
      aria-expanded={disclosure.expanded}
      onClick={() => setDisclosure({ finalStarted, expanded: !disclosure.expanded })}>
      <span>{label}</span><ChevronRight aria-hidden="true" className={disclosure.expanded ? "isExpanded" : ""} />
    </button> : <div className="workProgressStatus">{label}</div>}
    {disclosure.expanded ? <div className="workProgressBody">{children}</div> : null}
    <div hidden={responseIsProcess && !disclosure.expanded}>{response}</div>
  </div>;
}
