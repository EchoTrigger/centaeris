import { t } from "../../i18n";
import { useStreamPresentation } from "../../useStreamPresentation";
import { memo, useMemo, type ReactNode } from "react";
import { useShallow } from "zustand/react/shallow";
import { CircleAlert, CornerDownLeft } from "lucide-react";
import { runFailureDetail } from "../../runErrors";
import { MarkdownContent } from "./MarkdownContent";
import { useChatViewStore } from "./chatViewStore";
import { TaskGroupTranscriptItem } from "./ToolActivityTranscript";
import { ReasoningTranscript } from "./ReasoningTranscript";
import type {
  AgentResultStreamProps,
  SubagentResult,
  TranscriptItem,
  TranscriptProcessSection,
  TranscriptTextItem,
  TranscriptViewModel,
} from "./types";

type OpenWorkspacePath = AgentResultStreamProps["onOpenWorkspacePath"];

const renderProcessHeading = (entry: TranscriptTextItem, onOpenWorkspacePath: OpenWorkspacePath) =>
  entry.runFailure ? (
    <div className="agentRunError" role="status" key={entry.id}>
      <span className="agentRunErrorIcon" aria-hidden="true">
        <CircleAlert />
      </span>
      <div className="agentRunErrorText">{runFailureDetail(entry.text)}</div>
    </div>
  ) : (
    <div
      className={`agentProcessSectionHeading ${entry.tone === "error" ? "is-error" : ""} ${entry.phase === "compaction" ? "is-compaction" : ""}`}
      data-waterfall-section={entry.waterfall?.section ?? "process"}
      key={entry.id}
    >
      {entry.phase === "compaction" ? (
        entry.text
      ) : (
        <MarkdownContent text={entry.text} onOpenWorkspacePath={onOpenWorkspacePath} />
      )}
    </div>
  );

const renderTranscriptItem = (
  entry: TranscriptItem,
  onOpenWorkspacePath: OpenWorkspacePath,
  agentRunId: string | undefined,
) => {
  if (entry.kind === "reasoning") {
    return (
      <ReasoningTranscript
        entry={entry}
        scopeId={agentRunId ?? entry.turnId ?? entry.id}
        key={entry.id}
        onOpenWorkspacePath={onOpenWorkspacePath}
      />
    );
  }
  if (entry.kind === "guidedSupplement") {
    return (
      <div className="agentGuidedSupplement" data-waterfall-section="process" key={entry.id}>
        <div className="agentGuidedSupplementLabel">
          <CornerDownLeft className="agentGuidedSupplementIcon" aria-hidden="true" />
          <span>{t("agentTranscriptSections.conversationRedirected")}</span>
        </div>
        <div className="agentGuidedSupplementBubble">
          <MarkdownContent text={entry.text} onOpenWorkspacePath={onOpenWorkspacePath} />
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

const AgentTranscriptSection = memo(function AgentTranscriptSection({
  section,
  isCurrent,
  isStreaming,
  agentRunId,
  onOpenWorkspacePath,
}: {
  section: TranscriptProcessSection;
  isCurrent: boolean;
  isStreaming: boolean;
  agentRunId: string | undefined;
  onOpenWorkspacePath: OpenWorkspacePath;
}) {
  const heading = section.heading;
  const isProcess = !heading || !["streaming", "final"].includes(heading.phase);
  return (
    <section
      className={`agentProcessSection${isCurrent ? " agentCurrentTextSection" : ""}`}
      style={{
        display: !heading && !section.items.length ? "none" : undefined,
        width: isProcess ? "var(--agent-text-rail)" : "100%",
        marginInline: "auto",
      }}
    >
      {isCurrent || heading?.modelText ? (
        <AssistantTextReader
          finalItem={heading ?? null}
          isStreaming={isStreaming}
          isProcessText={isProcess}
          onOpenWorkspacePath={onOpenWorkspacePath}
        />
      ) : heading ? (
        renderProcessHeading(heading, onOpenWorkspacePath)
      ) : null}
      {section.items.length > 0 ? (
        <div className="agent-inline-feed unified-feed agentProcessFeed">
          {section.items.map((entry) =>
            renderTranscriptItem(entry, onOpenWorkspacePath, agentRunId),
          )}
        </div>
      ) : null}
    </section>
  );
});

// One keyed parent retains both the presentation queue and Markdown nodes when
// current model text becomes a committed process section.
export const AgentProcessTranscript = memo(function AgentProcessTranscript({
  processTranscript,
  currentTextId,
  finalItem,
  isStreaming = false,
  processFooter,
  agentRunId,
  onOpenWorkspacePath,
}: {
  processTranscript: Pick<TranscriptViewModel, "processItems" | "processSections">;
  currentTextId?: string;
  processFooter?: ReactNode;
  finalItem?: TranscriptViewModel["finalItem"];
  isStreaming?: boolean;
  agentRunId: string | undefined;
  onOpenWorkspacePath: OpenWorkspacePath;
}) {
  const currentSection = useMemo<TranscriptProcessSection | null>(
    () =>
      currentTextId
        ? {
            id: `section-${currentTextId}`,
            heading: finalItem ?? undefined,
            items: [],
          }
        : null,
    [currentTextId, finalItem],
  );
  const sections = currentSection
    ? [...processTranscript.processSections, currentSection]
    : processTranscript.processSections;
  if (!sections.length && !processFooter) return null;
  const readers = sections.map((section) => (
    <AgentTranscriptSection
      key={section.id}
      section={section}
      isCurrent={section === currentSection}
      isStreaming={section === currentSection && isStreaming}
      agentRunId={agentRunId}
      onOpenWorkspacePath={onOpenWorkspacePath}
    />
  ));
  if (processFooter)
    readers.splice(
      currentSection ? readers.length - 1 : readers.length,
      0,
      <div key="subagent-results" className="agentSubagentResults">
        {processFooter}
      </div>,
    );
  return (
    <div
      className="agentProcessLive"
      style={{
        width: currentTextId ? "100%" : undefined,
        display:
          !processTranscript.processItems.length && !finalItem && !processFooter
            ? "none"
            : undefined,
      }}
    >
      <div className={`agentProcessSections${currentTextId ? " agentReplySections" : ""}`}>
        {readers}
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
  const subagent = useChatViewStore(useShallow((state) => state.subagentById[entry.id] ?? entry));
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

export const AssistantTextReader = memo(function AssistantTextReader({
  finalItem,
  isStreaming,
  isProcessText = false,
  onOpenWorkspacePath,
}: {
  finalItem: TranscriptViewModel["finalItem"];
  isStreaming: boolean;
  isProcessText?: boolean;
  onOpenWorkspacePath: OpenWorkspacePath;
}) {
  const targetText = finalItem?.text ?? "";
  const visibleText = useStreamPresentation(targetText, isStreaming);
  if (!finalItem) {
    return null;
  }
  const presentationActive = isStreaming || visibleText !== targetText;
  if (finalItem.runFailure || finalItem.phase === "compaction")
    return renderProcessHeading(finalItem, onOpenWorkspacePath);
  return (
    <div
      className={
        isProcessText
          ? `agentProcessSectionHeading${presentationActive ? " isStreaming" : ""}`
          : "agentAssistantAnswer answerMarkdownBlock"
      }
      data-waterfall-section={
        isProcessText ? "process" : (finalItem?.waterfall?.section ?? "final")
      }
    >
      <div className="answer-content" key={finalItem.id}>
        <MarkdownContent
          text={visibleText}
          allowVisuals={!isProcessText || finalItem.modelText}
          isStreaming={presentationActive}
          onOpenWorkspacePath={onOpenWorkspacePath}
        />
      </div>
    </div>
  );
});
