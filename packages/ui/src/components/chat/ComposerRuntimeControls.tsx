import { Select, SelectContent, SelectGroup, SelectItem, SelectLabel, SelectTrigger } from "../ui/select";
import { Popover, PopoverContent, PopoverTrigger } from "../ui/popover";
import { t } from "../../i18n";
import { memo, useMemo, type CSSProperties } from "react";
import { Image } from "lucide-react";
import type {
  AgentContextUsageSummary,
  ModelThinkingMode,
  SelectableModel,
} from "../../lib/chatBridge";
import { formatTokenCount } from "./chatRuntimeCore";

export type RuntimeComposerPanel = "model" | "reasoning" | "context" | null;

export type ComposerRuntimeControlsProps = {
  activePanel: RuntimeComposerPanel;
  modelRuntimeSummary: string;
  selectableModels: SelectableModel[];
  activeModelIndex: number;
  reasoningEffort: ModelThinkingMode | null;
  reasoningEfforts: ModelThinkingMode[];
  contextUsage: AgentContextUsageSummary | null;
  runtimeConfigError: string;
  onPanelChange: (panel: RuntimeComposerPanel) => void;
  onModelSelect: (model: SelectableModel) => void;
  onReasoningEffortSelect: (effort: ModelThinkingMode) => void;
};

const formatContextTokenCount = (value: number): string =>
  new Intl.NumberFormat("en-US", {
    notation: "compact",
    maximumFractionDigits: 1,
  }).format(value).toLowerCase();

export const ComposerRuntimeControls = memo(function ComposerRuntimeControls({
  activePanel,
  modelRuntimeSummary,
  selectableModels,
  activeModelIndex,
  reasoningEffort,
  reasoningEfforts,
  contextUsage,
  runtimeConfigError,
  onPanelChange,
  onModelSelect,
  onReasoningEffortSelect,
}: ComposerRuntimeControlsProps) {
  const breakdown = contextUsage?.breakdown;
  const reasoningLabel = reasoningEffort
    ?? selectableModels[activeModelIndex]?.modelThinkingMode
    ?? null;
  const maxContextTokens = contextUsage?.maxContextTokens ?? 0;
  const usedTokens = contextUsage?.usedTokens ?? 0;
  const usedPercentage = contextUsage?.usedPercentage ?? 0;
  const contextSegments = breakdown ? [
    ["Messages", breakdown.messageTokens, "messages"],
    ["System tools", breakdown.systemToolTokens, "system-tools"],
    ["MCP tools", breakdown.mcpToolTokens, "mcp-tools"],
    ["System prompt", breakdown.systemPromptTokens, "system-prompt"],
    ["Skills", breakdown.skillsTokens, "skills"],
    ["buffer", breakdown.autoCompactBufferTokens, "buffer"],
    ["Free space", breakdown.freeSpaceTokens, "free"],
  ] as const : [];
  const contextRows = contextSegments.filter(([, , kind]) => kind !== "buffer");
  const activeModel = selectableModels[activeModelIndex];
  const modelGroups = useMemo(() => {
    const groups = new Map<string, {
      providerId: string;
      providerName: string;
      models: SelectableModel[];
    }>();
    selectableModels.forEach((model) => {
      const current = groups.get(model.providerId);
      if (current) {
        current.models.push(model);
      } else {
        groups.set(model.providerId, {
          providerId: model.providerId,
          providerName: model.providerName,
          models: [model],
        });
      }
    });
    return [...groups.values()];
  }, [selectableModels]);

  return (
    <>
      <div className="composerMetaRow">
        <div className="composerMetaGroup">
          <span className="composer-chip composerImageIndicator" title={t("composerRuntimeControls.image")}>
            <Image className="composerLucideIcon" aria-hidden="true" />
          </span>
          <Select value={activeModel ? JSON.stringify([activeModel.providerId, activeModel.model]) : null}
            open={activePanel === "model"}
            onOpenChange={(open) => onPanelChange(open ? "model" : null)}
            onValueChange={(value) => {
              const model = selectableModels.find((item) => JSON.stringify([item.providerId, item.model]) === value);
              if (model) onModelSelect(model);
            }}>
            <SelectTrigger className="composer-chip" title={modelRuntimeSummary} aria-label={t("composerRuntimeControls.globalModels")}>
              <span>{activeModelIndex >= 0 ? (activeModel?.displayName || activeModel?.model) : t("composerRuntimeControls.noModelConfigured")}</span>
            </SelectTrigger>
            <SelectContent side="top" align="start" alignItemWithTrigger={false} className="min-w-64">
              {modelGroups.map((group) => <SelectGroup key={group.providerId}>
                <SelectLabel>{group.providerName}</SelectLabel>
                {group.models.map((configured) => <SelectItem key={JSON.stringify([configured.providerId, configured.model])}
                  value={JSON.stringify([configured.providerId, configured.model])}>
                  {configured.displayName || configured.model}
                </SelectItem>)}
              </SelectGroup>)}
            </SelectContent>
          </Select>
        </div>
        <div className="composerMetaGroup is-end">
          {reasoningEfforts.length ? (
            <Select value={reasoningEffort} open={activePanel === "reasoning"}
              onOpenChange={(open) => onPanelChange(open ? "reasoning" : null)}
              onValueChange={(value) => { if (value !== null) onReasoningEffortSelect(value); }}>
              <SelectTrigger className="composer-chip" aria-label={t("composerRuntimeControls.reasoningEffort")}><span>{reasoningLabel}</span></SelectTrigger>
              <SelectContent side="top" align="end" alignItemWithTrigger={false}>
                {reasoningEfforts.map((effort) => <SelectItem key={effort} value={effort}>{effort}</SelectItem>)}
              </SelectContent>
            </Select>
          ) : reasoningLabel ? <span className="composer-chip reasoning-chip">{reasoningLabel}</span> : null}
          <Popover open={activePanel === "context"} onOpenChange={(open) => onPanelChange(open ? "context" : null)}>
          <div className={`composerPicker contextWindowPicker ${activePanel === "context" ? "is-open" : ""}`}>
            <PopoverTrigger
              className="contextWindowIndicator"
              aria-label="Context window"
              title="Context window"
              aria-expanded={activePanel === "context"}
            >
              <span style={{ "--context-used": `${usedPercentage * 3.6}deg` } as CSSProperties} />
            </PopoverTrigger>
              <PopoverContent side="top" align="end" className="contextWindowPanel">
                <header>
                  <span>Context window</span>
                  <strong title={`${formatTokenCount(usedTokens)} / ${formatTokenCount(maxContextTokens)}`}>
                    {formatContextTokenCount(usedTokens)} / {formatContextTokenCount(maxContextTokens)} ({usedPercentage}%)
                  </strong>
                </header>
                <div className="contextWindowBar">
                  {contextSegments.filter(([, tokens]) => tokens > 0).map(([label, tokens, kind]) => (
                    <i
                      key={label}
                      className={`is-${kind}`}
                      style={{ width: `${maxContextTokens ? (tokens / maxContextTokens) * 100 : 0}%` }}
                    />
                  ))}
                </div>
                {breakdown ? (
                  <>
                    <div className="contextWindowRows">
                      {contextRows.map(([label, tokens, kind]) => (
                        <div key={label}>
                          <i className={`is-${kind}`} />
                          <span>{label}</span>
                          <strong title={formatTokenCount(tokens)}>{formatContextTokenCount(tokens)}</strong>
                          <small>{maxContextTokens ? ((tokens / maxContextTokens) * 100).toFixed(1) : "0.0"}%</small>
                        </div>
                      ))}
                    </div>
                    {breakdown.mcpTools.length ? (
                      <details className="contextMcpTools">
                        <summary>
                          MCP tools
                          <span>{formatContextTokenCount(breakdown.mcpToolTokens)} · {breakdown.mcpTools.length}</span>
                        </summary>
                        <div>
                          {breakdown.mcpTools.map((tool) => (
                            <p key={`${tool.providerId}:${tool.name}`}>
                              <span title={`${tool.providerId} · ${tool.name}`}>{tool.providerId} · {tool.name}</span>
                              <strong title={formatTokenCount(tool.tokens)}>{formatContextTokenCount(tool.tokens)}</strong>
                            </p>
                          ))}
                        </div>
                      </details>
                    ) : null}
                  </>
                ) : null}
              </PopoverContent>
          </div>
          </Popover>
        </div>
      </div>
      {runtimeConfigError.trim() ? (
        <div className="composer-error">{runtimeConfigError}</div>
      ) : null}
    </>
  );
});
