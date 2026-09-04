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
  onOpenAgentSession,
  onOpenWorkspacePath,
}: AgentResultStreamProps) {
  const { chunks, finalAnswer, id, isStreaming } = turn;
  const processTranscript = useMemo(
    () => buildTranscriptProcessViewModel({ chunks }),
    [chunks],
  );
  const finalItem = useMemo(
    () => buildTranscriptFinalItem({ finalAnswer, id, isStreaming }),
    [finalAnswer, id, isStreaming],
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
  return (
    <div className="agentResultBash">
      <div className="agentResultMain">
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
        <AgentFinalAnswer
          finalItem={finalItem}
          onOpenWorkspacePath={onOpenWorkspacePath}
        />
      </div>
    </div>
  );
}
