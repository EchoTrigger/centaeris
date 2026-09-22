import { RunStatusLine } from "./RunStatusLine";
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
    () => buildTranscriptFinalItem({ finalAnswer: isStreaming && !turn.finalAnswerConfirmed ? "" : finalAnswer, id, isStreaming: false }),
    [finalAnswer, id, isStreaming, turn.finalAnswerConfirmed],
  );
  const subagents = useMemo(
    () => chunks
      .filter((chunk): chunk is SubagentChunk => chunk.kind === "subagent")
      .map((chunk) => chunk.subagent),
    [chunks],
  );
  const process = <>
        <AgentProcessTranscript
          processTranscript={processTranscript}
          agentRunId={turn.agentRunId}
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
        {process}
        <AgentFinalAnswer finalItem={finalItem} isStreaming={false} onOpenWorkspacePath={onOpenWorkspacePath} />
        <RunStatusLine turnId={turn.id} />
      </div>
    </div>
  );
}
