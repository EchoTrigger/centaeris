import { WorkProgress } from "./WorkProgress";
import { useMemo } from "react";
import {
  AgentFinalAnswer,
  AgentProcessTranscript,
  AgentSubagentTranscript,
} from "./AgentTranscriptSections";
import {
  buildTranscriptFinalItem,
  buildTranscriptProcessViewModel,
} from "./agentTranscriptModel";
import type {
  AgentResultStreamProps,
  SubagentChunk,
} from "./types";

export function AgentResultStream({
  turn,
  showWorkProgress = true,
  onOpenAgentSession,
  onOpenWorkspacePath,
}: AgentResultStreamProps) {
  const { chunks, finalAnswer, id, isStreaming } = turn;
  const processTranscript = useMemo(
    () => buildTranscriptProcessViewModel({ chunks }),
    [chunks],
  );
  const finalItem = useMemo(
    () => buildTranscriptFinalItem({ finalAnswer: isStreaming && !turn.finalAnswerConfirmed ? "" : finalAnswer, id, isStreaming: false }),
    [finalAnswer, id, isStreaming, turn.finalAnswerConfirmed],
  );
  const subagents = useMemo(
    () => chunks
      .filter((chunk): chunk is SubagentChunk => chunk.kind === "subagent")
      .map((chunk) => chunk.subagent),
    [chunks],
  );
  const hasRunningTool = chunks.some(
    (chunk) => chunk.kind === "task" && chunk.task.status === "running",
  );
  const isProcessDraft = isStreaming && !turn.finalAnswerConfirmed;
  const process = <>
        <AgentProcessTranscript
          processTranscript={processTranscript}
          isStreaming={isStreaming}
          agentRunId={turn.agentRunId}
          activity={turn.activity}
          subagents={subagents}
          hasRunningTool={hasRunningTool}
          hasFinalItem={Boolean(finalItem)}
          onOpenWorkspacePath={onOpenWorkspacePath}
        />
        <AgentSubagentTranscript
          subagents={subagents}
          onOpenAgentSession={onOpenAgentSession}
        />
  </>;
  return (
    <div className="agentResultBash">
      <div className="agentResultMain">
        {showWorkProgress ? <WorkProgress running={isStreaming} finalStarted={Boolean(finalItem) && !isProcessDraft}
          startedAtMs={turn.startedAtMs} completedAtMs={turn.completedAtMs}
          responseIsProcess={isProcessDraft}
          response={<AgentFinalAnswer finalItem={finalItem} isStreaming={false} onOpenWorkspacePath={onOpenWorkspacePath} />}>
          {process}
        </WorkProgress> : <>{process}<AgentFinalAnswer finalItem={finalItem} isStreaming={false} onOpenWorkspacePath={onOpenWorkspacePath} /></>}

      </div>
    </div>
  );
}
