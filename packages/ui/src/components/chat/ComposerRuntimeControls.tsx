import { memo, useMemo, type CSSProperties } from "react";
import { Check, ChevronDown, Image } from "lucide-react";
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
  onTogglePanel: (panel: Exclude<RuntimeComposerPanel, null>) => void;
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
  onTogglePanel,
  onModelSelect,
  onReasoningEffortSelect,
}: ComposerRuntimeControlsProps) {
  const breakdown = contextUsage?.breakdown;
  const reasoningLabel = reasoningEffort
    ?? selectableModels[activeModelIndex]?.modelThinkingMode
    ?? "provider default";
  const maxContextTokens = contextUsage?.maxContextTokens ?? 0;
  const usedTokens = contextUsage?.usedTokens ?? 0;
  const usedPercentage = contextUsage?.usedPercentage ?? 0;
  const contextRows = breakdown ? [
    ["Messages", breakdown.messageTokens, "messages"],
    ["System tools", breakdown.systemToolTokens, "system-tools"],
    ["MCP tools", breakdown.mcpToolTokens, "mcp-tools"],
    ["System prompt", breakdown.systemPromptTokens, "system-prompt"],
    ["Skills", breakdown.skillsTokens, "skills"],
    ["Autocompact buffer", breakdown.autoCompactBufferTokens, "buffer"],
    ["Free space", breakdown.freeSpaceTokens, "free"],
  ] as const : [];
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
          <span className="composer-chip composerImageIndicator" title="图片">
            <Image className="composerLucideIcon" aria-hidden="true" />
          </span>
          <div className={`composerPicker model-chip ${activePanel === "model" ? "is-open" : ""}`}>
            <button
              type="button"
              className="composer-chip composerPickerTrigger"
              title={modelRuntimeSummary}
              aria-haspopup="listbox"
              aria-expanded={activePanel === "model"}
              onClick={() => onTogglePanel("model")}
            >
              <span>{activeModelIndex >= 0 ? (activeModel?.displayName || activeModel?.model) : "未配置模型"}</span>
              <ChevronDown className="composerLucideIcon is-chevron" aria-hidden="true" />
            </button>
            {activePanel === "model" ? (
              <div className="composerPickerPanel is-model" role="listbox" aria-label="全局模型">
                {modelGroups.map((group) => (
                  <section key={group.providerId}>
                    <p className="composerPickerGroupLabel">{group.providerName}</p>
                    {group.models.map((configured) => {
                      const selected = configured.providerId === activeModel?.providerId
                        && configured.model === activeModel?.model;
                      return (
                        <button
                          type="button"
                          role="option"
                          aria-selected={selected}
                          className={selected ? "is-selected" : ""}
                          key={`${configured.providerId}:${configured.model}`}
                          onClick={() => onModelSelect(configured)}
                        >
                          <span className="composerPickerCheck">
                            {selected ? <Check aria-hidden="true" /> : null}
                          </span>
                          <strong>{configured.displayName || configured.model}</strong>
                        </button>
                      );
                    })}
                  </section>
                ))}
              </div>
            ) : null}
          </div>
        </div>
        <div className="composerMetaGroup is-end">
          {reasoningEfforts.length ? (
            <div className={`composerPicker reasoning-chip ${activePanel === "reasoning" ? "is-open" : ""}`}>
              <button
                type="button"
                className="composer-chip composerPickerTrigger"
                aria-label="思考强度"
                aria-haspopup="listbox"
                aria-expanded={activePanel === "reasoning"}
                onClick={() => onTogglePanel("reasoning")}
              >
                <span>{reasoningLabel}</span>
                <ChevronDown className="composerLucideIcon is-chevron" aria-hidden="true" />
              </button>
              {activePanel === "reasoning" ? (
                <div className="composerPickerPanel is-reasoning" role="listbox" aria-label="思考强度">
                  {reasoningEfforts.map((effort) => {
                    const selected = effort === reasoningEffort;
                    return (
                      <button
                        type="button"
                        role="option"
                        aria-selected={selected}
                        className={selected ? "is-selected" : ""}
                        key={effort}
                        onClick={() => onReasoningEffortSelect(effort)}
                      >
                        <span className="composerPickerCheck">
                          {selected ? <Check aria-hidden="true" /> : null}
                        </span>
                        <strong>{effort}</strong>
                      </button>
                    );
                  })}
                </div>
              ) : null}
            </div>
          ) : (
            <span className="composer-chip reasoning-chip">{reasoningLabel}</span>
          )}
          <div className={`composerPicker contextWindowPicker ${activePanel === "context" ? "is-open" : ""}`}>
            <button
              type="button"
              className="contextWindowIndicator"
              aria-label="Context window"
              title="Context window"
              aria-expanded={activePanel === "context"}
              onClick={() => onTogglePanel("context")}
            >
              <span style={{ "--context-used": `${usedPercentage * 3.6}deg` } as CSSProperties} />
            </button>
            {activePanel === "context" ? (
              <div className="contextWindowPanel">
                <header>
                  <span>Context window</span>
                  <strong title={`${formatTokenCount(usedTokens)} / ${formatTokenCount(maxContextTokens)}`}>
                    {formatContextTokenCount(usedTokens)} / {formatContextTokenCount(maxContextTokens)} ({usedPercentage}%)
                  </strong>
                </header>
                <div className="contextWindowBar">
                  {contextRows.filter(([, tokens]) => tokens > 0).map(([label, tokens, kind]) => (
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
              </div>
            ) : null}
          </div>
        </div>
      </div>
      {runtimeConfigError.trim() ? (
        <div className="composer-error">{runtimeConfigError}</div>
      ) : null}
    </>
  );
});
