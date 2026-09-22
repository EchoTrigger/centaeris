import { t } from "../../i18n";
import { useStreamPresentation } from "../../useStreamPresentation";
import { memo } from "react";
import { useShallow } from "zustand/react/shallow";
import { CornerDownLeft } from "lucide-react";
import { MarkdownContent } from "./MarkdownContent";
import { useChatViewStore } from "./chatViewStore";
import { TaskGroupTranscriptItem } from "./ToolActivityTranscript";
import { ReasoningTranscript } from "./ReasoningTranscript";
import type {
  AgentResultStreamProps,
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
  agentRunId: string | undefined,
) => {
  if (entry.kind === "reasoning") {
    return <ReasoningTranscript entry={entry} scopeId={agentRunId ?? entry.turnId ?? entry.id}
      key={entry.id} onOpenWorkspacePath={onOpenWorkspacePath} />;
  }
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
          <span>{t("agentTranscriptSections.conversationRedirected")}</span>
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
  agentRunId,
  onOpenWorkspacePath,
}: {
  processTranscript: Pick<TranscriptViewModel, "processItems" | "processSections">;
  agentRunId: string | undefined;
  onOpenWorkspacePath: OpenWorkspacePath;
}) {
  if (processTranscript.processItems.length === 0) return null;

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
                  renderTranscriptItem(entry, onOpenWorkspacePath, agentRunId)
                )}
              </div>
            ) : null}
          </section>
        ))}
      </div>
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
    <div className="agentSubagentTags" aria-label={t("agentTranscriptSections.agentConversation")}>
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
  isStreaming,
  isProcessDraft = false,
  onOpenWorkspacePath,
}: {
  finalItem: TranscriptViewModel["finalItem"];
  isStreaming: boolean;
  isProcessDraft?: boolean;
  onOpenWorkspacePath: OpenWorkspacePath;
}) {
  const targetText = finalItem?.text ?? "";
  const visibleText = useStreamPresentation(targetText, isStreaming);
  if (!finalItem) {
    return null;
  }
  const presentationActive = isStreaming || visibleText !== targetText;
  return (
    <div
      className={isProcessDraft ? "agentProcessSectionHeading isStreaming" : "agentAssistantAnswer answerMarkdownBlock"}
      data-waterfall-section={isProcessDraft ? "process" : finalItem?.waterfall?.section ?? "final"}
    >
      <div className="answer-content" key={finalItem.id}>
        <MarkdownContent
          text={visibleText}
          isStreaming={presentationActive}
          onOpenWorkspacePath={onOpenWorkspacePath}
        />
      </div>
    </div>
  );
});
