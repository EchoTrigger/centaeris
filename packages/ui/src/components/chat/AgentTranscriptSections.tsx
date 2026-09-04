import { memo } from "react";
import { useShallow } from "zustand/react/shallow";
import { CornerDownLeft } from "lucide-react";
import { MarkdownContent } from "./MarkdownContent";
import {
  runtimeEasterEgg,
  tachikomaEasterEgg,
} from "./chatRuntimeCore";
import { useChatViewStore } from "./chatViewStore";
import { TaskGroupTranscriptItem } from "./ToolActivityTranscript";
import type {
  AgentResultStreamProps,
  RuntimeActivity,
  SubagentResult,
  TranscriptItem,
  TranscriptTextItem,
  TranscriptViewModel,
} from "./types";

type OpenWorkspacePath = AgentResultStreamProps["onOpenWorkspacePath"];

const renderProcessHeading = (
  entry: TranscriptTextItem,
  onOpenWorkspacePath: OpenWorkspacePath,
) => (
  <div
    className={`agentProcessSectionHeading ${entry.tone === "error" ? "is-error" : ""} ${entry.phase === "compaction" ? "is-compaction" : ""}`}
    data-waterfall-section={entry.waterfall?.section ?? "process"}
    key={entry.id}
  >
    {entry.phase === "compaction" ? entry.text : (
      <MarkdownContent
        text={entry.text}
        onOpenWorkspacePath={onOpenWorkspacePath}
      />
    )}
  </div>
);

const renderTranscriptItem = (
  entry: TranscriptItem,
  onOpenWorkspacePath: OpenWorkspacePath,
) => {
  if (entry.kind === "guidedSupplement") {
    return (
      <div
        className="agentGuidedSupplement"
        data-waterfall-section="process"
        key={entry.id}
      >
        <div className="agentGuidedSupplementLabel">
          <CornerDownLeft
            className="agentGuidedSupplementIcon"
            aria-hidden="true"
          />
          <span>已引导对话</span>
        </div>
        <div className="agentGuidedSupplementBubble">
          <MarkdownContent
            text={entry.text}
            onOpenWorkspacePath={onOpenWorkspacePath}
          />
        </div>
      </div>
    );
  }
  if (entry.kind === "assistantText") {
    return renderProcessHeading(entry, onOpenWorkspacePath);
  }

  return (
    <TaskGroupTranscriptItem
      entry={entry}
      key={entry.id}
      onOpenWorkspacePath={onOpenWorkspacePath}
    />
  );
};

export const AgentProcessTranscript = memo(function AgentProcessTranscript({
  processTranscript,
  isStreaming,
  agentRunId,
  activity,
  subagents,
  hasRunningTool,
  hasFinalItem,
  onOpenWorkspacePath,
}: {
  processTranscript: Pick<TranscriptViewModel, "processItems" | "processSections">;
  isStreaming: boolean;
  agentRunId: string | undefined;
  activity: RuntimeActivity | null | undefined;
  subagents: SubagentResult[];
  hasRunningTool: boolean;
  hasFinalItem: boolean;
  onOpenWorkspacePath: OpenWorkspacePath;
}) {
  const subagentIds = new Set(subagents.map((subagent) => subagent.subagentId));
  const liveSubagentIds = new Set(
    subagents
      .filter((subagent) => subagent.status === "running")
      .map((subagent) => subagent.subagentId),
  );
  const tachikomaCount = tachikomaEasterEgg(
    agentRunId,
    subagentIds.size,
    liveSubagentIds.size,
  );
  const hasTachikoma = tachikomaCount !== null;
  const activityLabel = runtimeEasterEgg(
    agentRunId,
    activity?.processState,
  ) ?? activity?.label ?? "";
  const liveStatus = isStreaming && (hasTachikoma || activity) &&
    (hasTachikoma || !hasRunningTool) && !hasFinalItem ? (
    <div className="agentStatusRow">
      <div className="agentRunStatus" aria-live="polite">
        <span className="agentRunStatusText">
          {hasTachikoma ? (
            <>
              Tachikoma{" "}
              <span className="tachikomaCount" key={tachikomaCount}>
                ×{tachikomaCount}
              </span>
              {tachikomaCount === 1 ? " · awaiting result…" : " · whispering…"}
            </>
          ) : activityLabel}
        </span>
      </div>
    </div>
  ) : null;

  if (processTranscript.processItems.length === 0 && !liveStatus) {
    return null;
  }

  return (
    <div className="agentProcessLive">
      <div className="agentProcessSections">
        {processTranscript.processSections.map((section) => (
          <section className="agentProcessSection" key={section.id}>
            {section.heading
              ? renderProcessHeading(section.heading, onOpenWorkspacePath)
              : null}
            {section.items.length > 0 ? (
              <div className="agent-inline-feed unified-feed agentProcessFeed">
                {section.items.map((entry) =>
                  renderTranscriptItem(entry, onOpenWorkspacePath)
                )}
              </div>
            ) : null}
          </section>
        ))}
      </div>
      {liveStatus}
    </div>
  );
});

const SubagentTranscriptTag = memo(function SubagentTranscriptTag({
  entry,
  onOpenAgentSession,
}: {
  entry: SubagentResult;
  onOpenAgentSession: AgentResultStreamProps["onOpenAgentSession"];
}) {
  const subagent = useChatViewStore(
    useShallow((state) => state.subagentById[entry.id] ?? entry),
  );
  const title = subagent.description?.trim() || subagent.title;
  const canOpen = Boolean(subagent.childSessionId && onOpenAgentSession);
  return (
    <button
      type="button"
      className={`agentSubagentTag is-${subagent.status}`}
      disabled={!canOpen}
      onClick={() => {
        if (subagent.childSessionId) {
          onOpenAgentSession?.(subagent.childSessionId, title);
        }
      }}
    >
      <span className="agentSubagentTagMark" aria-hidden="true" />
      <span className="agentSubagentTagRole">Agent</span>
      <span className="agentSubagentTagTitle">{title}</span>
    </button>
  );
});

export const AgentSubagentTranscript = memo(function AgentSubagentTranscript({
  subagents,
  onOpenAgentSession,
}: {
  subagents: SubagentResult[];
  onOpenAgentSession: AgentResultStreamProps["onOpenAgentSession"];
}) {
  if (subagents.length === 0) {
    return null;
  }
  return (
    <div className="agentSubagentTags" aria-label="Agent 会话">
      {subagents.map((subagent) => (
        <SubagentTranscriptTag
          entry={subagent}
          key={subagent.id}
          onOpenAgentSession={onOpenAgentSession}
        />
      ))}
    </div>
  );
});

export const AgentFinalAnswer = memo(function AgentFinalAnswer({
  finalItem,
  onOpenWorkspacePath,
}: {
  finalItem: TranscriptViewModel["finalItem"];
  onOpenWorkspacePath: OpenWorkspacePath;
}) {
  if (!finalItem) {
    return null;
  }
  return (
    <div
      className="agentAssistantAnswer answerMarkdownBlock"
      data-waterfall-section={finalItem.waterfall?.section ?? "final"}
    >
      <div className="answer-content" key={finalItem.id}>
        <MarkdownContent
          text={finalItem.text}
          isStreaming={finalItem.phase === "streaming"}
          onOpenWorkspacePath={onOpenWorkspacePath}
        />
      </div>
    </div>
  );
});
