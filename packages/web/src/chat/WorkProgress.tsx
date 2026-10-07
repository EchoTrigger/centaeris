import { useEffect, useLayoutEffect, useState, type ReactNode } from "react";
import { ChevronRight } from "lucide-react";
import { useTranslation } from "../i18n";
import { AnimatedDisclosure } from "./AnimatedDisclosure";
import { initialWorkDisclosure, observeFinalAnswer, toggleWorkDisclosure, preserveWorkDisclosure, type WorkDisclosure } from "./workDisclosure";
import { formatWorkDuration } from "./workDuration";

export function WorkProgress({ running, finalStarted, startedAtMs, completedAtMs, children, response, responseIsProcess = false, initialDisclosure, onDisclosureChange }: {
  initialDisclosure?: WorkDisclosure;
  onDisclosureChange?(value: WorkDisclosure): void;
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
  const [disclosure, setDisclosure] = useState(() => initialDisclosure ?? initialWorkDisclosure(finalStarted));
  useLayoutEffect(() => { onDisclosureChange?.(disclosure); }, [disclosure, onDisclosureChange]);
  const nextDisclosure = observeFinalAnswer(disclosure, finalStarted);
  if (nextDisclosure !== disclosure) setDisclosure(nextDisclosure);
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
    {disclosure.finalSeen ? <button className="workProgressSummary workProgressHeader" type="button" aria-label={label}
      aria-expanded={disclosure.expanded}
      onClick={() => setDisclosure(toggleWorkDisclosure)}>
      <span>{label}</span><ChevronRight aria-hidden="true" className={disclosure.expanded ? "isExpanded" : ""} />
    </button> : <div className="workProgressStatus workProgressHeader">{label}</div>}
    <AnimatedDisclosure expanded={disclosure.expanded}><div className="workProgressBody" onClickCapture={(event) => {
      // Opening any process detail is a deliberate reading choice, including
      // keyboard activation. A later final answer must not interrupt it.
      if (event.target instanceof Element && event.target.closest('button[aria-expanded="false"]')) {
        setDisclosure(preserveWorkDisclosure);
      }
    }}>{children}</div></AnimatedDisclosure>
    <div hidden={responseIsProcess && !disclosure.expanded}>{response}</div>
  </div>;
}
