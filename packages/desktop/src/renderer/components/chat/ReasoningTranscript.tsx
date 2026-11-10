import { AnimatedDisclosure } from "./AnimatedDisclosure";
import { useTranslation } from "../../i18n";
import { memo, useId } from "react";
import { Brain, ChevronDown } from "lucide-react";
import { MarkdownContent } from "./MarkdownContent";
import { useReasoningFollow } from "./useReasoningFollow";
import { useChatViewStore } from "./chatViewStore";
import type { AgentResultStreamProps, ReasoningChunk } from "./types";

export const ReasoningTranscript = memo(function ReasoningTranscript({
  entry, scopeId, onOpenWorkspacePath,
}: {
  entry: ReasoningChunk;
  scopeId: string;
  onOpenWorkspacePath?: AgentResultStreamProps["onOpenWorkspacePath"];
}) {
  const identity = JSON.stringify([scopeId, entry.id]);
  const expanded = useChatViewStore((state) => Boolean(state.expandedReasoning[identity]));
  const toggle = useChatViewStore((state) => state.toggleReasoning);
  const bodyId = useId();
  const bodyRef = useReasoningFollow(expanded, entry.status === "streaming");
  const { t } = useTranslation();
  const label = !expanded && entry.status === "streaming" ? t("reasoningTranscript.thinking") : t("reasoningTranscript.thoughts");
  return (
    <div className="agentReasoning" data-waterfall-section="process">
      <button type="button" className="agentReasoningSummary"
        aria-label={label}
        aria-expanded={expanded} aria-controls={bodyId} onClick={() => toggle(identity)}>
        <Brain aria-hidden="true" />
        <span>{label}</span>
        <ChevronDown className={expanded ? "is-expanded" : ""} aria-hidden="true" />
      </button>
      <AnimatedDisclosure expanded={expanded}><div ref={bodyRef} id={bodyId} className="agentReasoningBody" role="region" aria-label={t("reasoningTranscript.thinkingContent")} tabIndex={0}>
        <MarkdownContent text={entry.text} onOpenWorkspacePath={onOpenWorkspacePath} />
      </div></AnimatedDisclosure>
    </div>
  );
});
