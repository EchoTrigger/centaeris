import { assistantTextIdentity } from "./assistantTextIdentity";
import { RunStatusLine } from "./RunStatusLine";
import { useMemo } from "react";
import { AgentProcessTranscript, AgentSubagentTranscript } from "./AgentTranscriptSections";
import { buildTranscriptFinalItem, buildTranscriptProcessViewModel } from "./agentTranscriptModel";
import type { AgentResultStreamProps, SubagentChunk } from "./types";

export function AgentResultStream({
  turn,
  onOpenAgentSession,
  onOpenWorkspacePath,
}: AgentResultStreamProps) {
  const { chunks, finalAnswer, id, isStreaming, textSegmentId } = turn;
  const processTranscript = useMemo(() => buildTranscriptProcessViewModel({ chunks }), [chunks]);
  const finalItem = useMemo(
    () => buildTranscriptFinalItem({ finalAnswer, id, isStreaming, textSegmentId }),
    [finalAnswer, id, isStreaming, textSegmentId],
  );
  const subagents = useMemo(
    () =>
      chunks
        .filter((chunk): chunk is SubagentChunk => chunk.kind === "subagent")
        .map((chunk) => chunk.subagent),
    [chunks],
  );
  const process = (
    <>
      <AgentProcessTranscript
        processTranscript={processTranscript}
        currentTextId={assistantTextIdentity(turn)}
        finalItem={finalItem}
        isStreaming={isStreaming}
        processFooter={
          subagents.length ? (
            <AgentSubagentTranscript
              subagents={subagents}
              onOpenAgentSession={onOpenAgentSession}
            />
          ) : null
        }
        agentRunId={turn.agentRunId}
        onOpenWorkspacePath={onOpenWorkspacePath}
      />
    </>
  );
  return (
    <div className="agentResultBash">
      <div className="agentResultMain">
        {process}
        <RunStatusLine turnId={turn.id} />
      </div>
    </div>
  );
}
