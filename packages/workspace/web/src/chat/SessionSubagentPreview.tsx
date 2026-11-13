import { useEffect, useState } from "react";
import { apiResponse } from "../api";
import { useTranslation } from "../i18n";
import { MarkdownContent } from "./MarkdownContent";
import { ReasoningBlock } from "./ReasoningBlock";
import { SessionSubagentStatus } from "./SessionSubagentPresentation";
import { TranscriptMessageMeta } from "./TranscriptMessageMeta";
import { TranscriptToolNode, type ToolDisplayStatus } from "./TranscriptBlockContent";
import { WorkProgress } from "./WorkProgress";
import { parseSessionSubagent, type SessionSubagent, type SessionSubagentMessage } from "./sessionSubagents";

const toolResultStatuses: Readonly<Record<string, ToolDisplayStatus>> = {
  successWithOutput: "completed", successNoOutput: "completed", successNoMatches: "completed",
  failed: "failed", denied: "failed", aborted: "interrupted",
};

function UserMessage({ text, createdAtMs }: { text: string; createdAtMs: number }) {
  return <div className="workspaceTranscriptBlock workspaceTranscriptUser">
    <div className="workspaceUserMessage">{text}</div>
    <TranscriptMessageMeta user text={text} reference={null} sessionId={null} projectionGeneration={null} observedAtMs={createdAtMs} />
  </div>;
}

function ChildReasoning({ message }: { message: SessionSubagentMessage }) {
  const [expanded, setExpanded] = useState(false);
  return <ReasoningBlock block={{ id: message.messageId, sequence: 0, text: message.reasoningContent ?? "", status: "done" }}
    expanded={expanded} onToggle={() => setExpanded(value => !value)} />;
}

function ChildMessage({ message, toolResults, running, answer = false }: {
  message: SessionSubagentMessage;
  toolResults: ReadonlyMap<string, SessionSubagentMessage>;
  running: boolean;
  answer?: boolean;
}) {
  if (message.role === "tool" && message.toolCallId && toolResults.get(message.toolCallId) === message) return null;
  if (message.role === "user") return <UserMessage text={message.content} createdAtMs={message.createdAtMs} />;
  return <div className="workspaceTranscriptBlock workspaceTranscriptAssistant">
    {message.reasoningContent ? <ChildReasoning message={message} /> : null}
    {message.content ? <>
      <div className={answer ? "workspaceTerminalAnswer" : "workspaceStageSummary"}><MarkdownContent text={message.content} allowVisuals /></div>
      {answer ? <TranscriptMessageMeta answer text={message.content} reference={null} sessionId={null} projectionGeneration={null} observedAtMs={message.createdAtMs} /> : null}
    </> : null}
    {message.toolCalls.length ? <div className="agent-tool-node-list">{message.toolCalls.map(call => {
      const result = toolResults.get(call.id);
      let normalizedInput: unknown;
      let argumentError: string | undefined;
      try { normalizedInput = JSON.parse(call.argsJson); } catch (error) {
        if (!(error instanceof SyntaxError)) throw error;
        argumentError = `${error.message}\n${call.argsJson}`;
      }
      const status = result ? toolResultStatuses[result.resultState ?? ""] ?? "unconfirmed" : running && !argumentError ? "running" : "unconfirmed";
      return <TranscriptToolNode key={call.id} call={{ toolName: call.name, normalizedInput }}
        operation={{ toolName: call.name, modelContent: result?.content, resultState: result?.resultState, error: argumentError }} outputRef={null} status={status} />;
    })}</div> : null}
  </div>;
}

export function SessionSubagentPreview({ sessionId, agent, revision }: {
  sessionId: string;
  agent: SessionSubagent;
  revision: string;
}) {
  const { t } = useTranslation();
  const [snapshot, setSnapshot] = useState<{ sessionId: string; runtimeJobId: string; detail: ReturnType<typeof parseSessionSubagent> } | null>(null);
  const [error, setError] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const [busy, setBusy] = useState(true);
  useEffect(() => {
    // A committed child snapshot can advance without changing the Job status.
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
        const detail = parseSessionSubagent(value, sessionId, agent.runtimeJobId);
        if (!controller.signal.aborted) setSnapshot({ sessionId, runtimeJobId: agent.runtimeJobId, detail });
      })
      .catch(() => { if (!controller.signal.aborted) setError(true); })
      .finally(() => { if (!controller.signal.aborted) setBusy(false); });
    return () => controller.abort();
  }, [sessionId, agent.runtimeJobId, agent.updatedAtMs, agent.status, revision, attempt]);
  const detail = snapshot?.sessionId === sessionId && snapshot.runtimeJobId === agent.runtimeJobId ? snapshot.detail : null;
  const currentAgent = detail?.agent ?? agent;
  const messages = detail?.messages ?? [];
  const running = ["queued", "leased", "running"].includes(currentAgent.status);
  const toolResults = new Map<string, SessionSubagentMessage>();
  const calledTools = new Set(messages.flatMap(message => message.toolCalls.map(call => call.id)));
  for (const message of messages) {
    if (message.role === "tool" && message.toolCallId && calledTools.has(message.toolCallId)) toolResults.set(message.toolCallId, message);
  }
  const initialUser = messages.findIndex(message => message.role === "user");
  const lastMessage = messages[messages.length - 1];
  const answer = currentAgent.status === "succeeded" && lastMessage?.role === "assistant" && lastMessage.toolCalls.length === 0 ? lastMessage : null;
  return <div className="sessionSubagentConversation" aria-busy={busy}>
    <div className="workspaceTranscriptBlocks">
      <SessionSubagentStatus agent={currentAgent} />
      <UserMessage text={currentAgent.prompt} createdAtMs={currentAgent.createdAtMs} />
      <WorkProgress running={running} finalStarted={answer !== null}
        hasProcess={initialUser >= 0 || messages.some(message => message !== answer && (Boolean(message.content || message.reasoningContent) || message.toolCalls.length > 0))}
        startedAtMs={currentAgent.createdAtMs}
        completedAtMs={running ? undefined : currentAgent.updatedAtMs}
        response={answer ? <ChildMessage message={answer} toolResults={toolResults} running={false} answer /> : undefined}>
        {initialUser >= 0 ? <details className="sessionSubagentTaskContext"><summary>{t("sessionSubagent.taskContext")}</summary>
          <div className="workspaceUserMessage">{messages[initialUser].content}</div>
        </details> : null}
        {messages.map((message, index) => index === initialUser || message === answer ? null
          : <ChildMessage key={message.messageId} message={message} toolResults={toolResults} running={running} />)}
      </WorkProgress>
      {error ? <p className="workspaceToolOutputStatus isError" role="alert">{t("sessionSubagent.unavailable")} <button type="button" onClick={() => setAttempt(value => value + 1)}>{t("agentRunRow.reload")}</button></p> : null}
    </div>
  </div>;
}
