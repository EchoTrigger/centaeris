
import { useTranslation } from "../i18n";
import { memo, useId } from "react";
import { Brain, ChevronDown } from "lucide-react";
import { MarkdownContent } from "./MarkdownContent";
import { reasoningPreview } from "./reasoningPreview";
import { useReasoningFollow } from "./useReasoningFollow";
import type { ReasoningBlockView } from "./sessionEvents";

export const ReasoningBlock = memo(function ReasoningBlock({ block, expanded, onToggle }: {
  block: ReasoningBlockView;
  expanded: boolean;
  onToggle: () => void;
}) {
  const { t } = useTranslation();
  const bodyId = useId();
  const bodyRef = useReasoningFollow(expanded, block.status === "streaming");
  const label = !expanded && block.status === "streaming" ? t("reasoningBlock.thinking") : t("reasoningBlock.thoughts");
  return (
    <div className="workspaceReasoning">
      <button type="button" className="workspaceActivityGroup"
        aria-label={label}
        aria-expanded={expanded} aria-controls={bodyId} onClick={onToggle}>
        <Brain aria-hidden="true" />
        <span className={!expanded && block.status === "streaming" ? "statusShimmer" : undefined}>{label}</span>
        {!expanded ? <span className="reasoningPreview" aria-hidden="true"><span className="reasoningPreviewText">{reasoningPreview(block.text)}</span></span> : null}
        <ChevronDown className={`workspaceActivityGroupChevron ${expanded ? "isExpanded" : ""}`} aria-hidden="true" />
      </button>
      {expanded ? <div ref={bodyRef} id={bodyId} className="workspaceReasoningBody" role="region" aria-label={t("reasoningBlock.thinkingContent")} tabIndex={0}><MarkdownContent text={block.text} /></div> : null}
    </div>
  );
});
