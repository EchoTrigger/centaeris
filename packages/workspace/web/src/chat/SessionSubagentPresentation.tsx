import { memo, useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { CircleCheck, CircleDashed, CircleStop, CircleX, LoaderCircle } from "lucide-react";
import { apiResponse } from "../api";
import { useTranslation } from "../i18n";
import { WorkspaceDialog } from "../components/WorkspaceDialog";
import { MarkdownContent } from "./MarkdownContent";
import { parseSessionSubagent, sortSessionSubagents, type SessionSubagent, type SessionSubagentMessage } from "./sessionSubagents";
import "./session-subagents.css";

const gridColors = ["#86d6ae", "#b5d98a", "#e6d27e", "#e8ae78"];
const statusIcons = { running: LoaderCircle, queued: CircleDashed, leased: CircleDashed,
  succeeded: CircleCheck, failed: CircleX, dead_lettered: CircleX, cancelled: CircleStop };
const toolResultLabels: Readonly<Record<string, string>> = {
  successWithOutput: "completed", successNoOutput: "completed", successNoMatches: "completed",
  failed: "failed", denied: "failed", aborted: "interrupted",
};

function AgentGridMark({ seed }: { seed: string }) {
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
  onOpen(agent: SessionSubagent): void;
}) {
  const { t } = useTranslation();
  return <div className="sessionSubagentTags" aria-label={t("sessionSubagent.conversations")}>
    {sortSessionSubagents(agents).map(agent => <button type="button" className="sessionSubagentTag" key={agent.runtimeJobId}
      aria-label={`Agent ${agent.description}: ${t(`sessionSubagent.status.${agent.status}`)}`} onClick={() => onOpen(agent)}>
      <AgentGridMark seed={agent.childSessionId} />
      <span className="sessionSubagentRole">Agent</span>
      <span className="sessionSubagentTitle">{agent.description}</span>
      <SessionSubagentStatus agent={agent} />
    </button>)}
  </div>;
});

function ChildMessage({ message, toolResults }: {
  message: SessionSubagentMessage;
  toolResults: ReadonlyMap<string, SessionSubagentMessage>;
}) {
  const { t } = useTranslation();
  if (message.role === "tool" && message.toolCallId && toolResults.get(message.toolCallId) === message) return null;
  return <div className={`sessionSubagentMessage is-${message.role}`}>
    {message.reasoningContent ? <details className="sessionSubagentReasoning">
      <summary>{t("reasoningBlock.thoughts")}</summary>
      <MarkdownContent text={message.reasoningContent} />
    </details> : null}
    {message.content ? message.role === "assistant" ? <MarkdownContent text={message.content} />
      : <pre className="sessionSubagentMessageText">{message.content}</pre> : null}
    {message.toolCalls.map(call => {
      const result = toolResults.get(call.id);
      return <details className="agent-operation-group sessionSubagentTool" key={call.id}>
        <summary className="agent-operation-summary"><span>{call.name}</span>
          {result ? <span className="agent-tool-node-status">{t(`transcriptTool.status.${toolResultLabels[result.resultState ?? ""] ?? "unconfirmed"}`)}</span> : null}
        </summary>
        <div className="agent-operation-body">
          <pre className="agent-tool-output-block">{call.argsJson}</pre>
          {result?.content ? <pre className="agent-tool-output-block">{result.content}</pre> : null}
        </div>
      </details>;
    })}
  </div>;
}

export function SessionSubagentDialog({ sessionId, agent, revision, onClose }: {
  sessionId: string;
  agent: SessionSubagent;
  revision: string;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const [detail, setDetail] = useState<ReturnType<typeof parseSessionSubagent> | null>(null);
  const [error, setError] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const [busy, setBusy] = useState(true);
  useEffect(() => {
    // Committed child snapshots can advance while the Job stays running.
    void revision;
    void agent.updatedAtMs;
    void agent.status;
    void attempt;
    const controller = new AbortController();
    setBusy(true);
    setError(false);
    void apiResponse(`/api/sessions/${encodeURIComponent(sessionId)}/subagents/${encodeURIComponent(agent.runtimeJobId)}`, { signal: controller.signal })
      .then(response => response.json())
      .then(value => {
        const next = parseSessionSubagent(value, sessionId, agent.runtimeJobId);
        if (!controller.signal.aborted) setDetail(next);
      })
      .catch(() => { if (!controller.signal.aborted) setError(true); })
      .finally(() => { if (!controller.signal.aborted) setBusy(false); });
    return () => controller.abort();
  }, [sessionId, agent.runtimeJobId, agent.updatedAtMs, agent.status, revision, attempt]);
  const messages = detail?.messages ?? [];
  const toolResults = new Map<string, SessionSubagentMessage>();
  const calledTools = new Set(messages.flatMap(message => message.toolCalls.map(call => call.id)));
  for (const message of messages) {
    if (message.role === "tool" && message.toolCallId && calledTools.has(message.toolCallId)) toolResults.set(message.toolCallId, message);
  }
  // Keep the complete initial input available alongside the concise task brief;
  // delegated constraints and parent context must not disappear from the view.
  const initialUser = messages.findIndex(message => message.role === "user");
  return createPortal(<WorkspaceDialog title={agent.description} description={undefined} wide className="sessionSubagentDialog" onClose={onClose}>
    <SessionSubagentStatus agent={agent} />
    <div className="sessionSubagentMessages" aria-busy={busy}>
      <div className="sessionSubagentPrompt">{agent.prompt}</div>
      {initialUser >= 0 ? <details className="sessionSubagentReasoning"><summary>{t("sessionSubagent.taskContext")}</summary>
        <pre className="sessionSubagentMessageText">{messages[initialUser].content}</pre>
      </details> : null}
      {messages.map((message, index) => index === initialUser ? null : <ChildMessage key={message.messageId} message={message} toolResults={toolResults} />)}
      {error ? <p className="dialogError" role="alert">{t("sessionSubagent.unavailable")} <button type="button" onClick={() => setAttempt(value => value + 1)}>{t("agentRunRow.reload")}</button></p> : null}
    </div>
  </WorkspaceDialog>, document.body);
}
