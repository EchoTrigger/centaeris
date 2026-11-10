import { memo, type CSSProperties } from "react";
import { ChevronDown } from "lucide-react";
import { t } from "../../i18n";
import type {
  AgentContextUsageSummary,
  ModelThinkingMode,
  SelectableModel,
} from "../../lib/chatBridge";
import { Button } from "../ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubTrigger,
  DropdownMenuSubContent,
} from "../ui/dropdown-menu";
import { Popover, PopoverContent, PopoverTrigger } from "../ui/popover";
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
const modelKey = (model: SelectableModel) =>
  JSON.stringify([model.providerId, model.model]);
const effortLabel = (effort: string) =>
  effort === "xhigh"
    ? "Extra high"
    : effort.charAt(0).toUpperCase() + effort.slice(1);
const formatContextTokenCount = (value: number) =>
  new Intl.NumberFormat("en-US", {
    notation: "compact",
    maximumFractionDigits: 1,
  })
    .format(value)
    .toLowerCase();

export const ComposerRuntimeControls = memo(function ComposerRuntimeControls({
  activePanel,
  selectableModels,
  activeModelIndex,
  reasoningEffort,
  reasoningEfforts,
  onPanelChange,
  onModelSelect,
  onReasoningEffortSelect,
}: ComposerRuntimeControlsProps) {
  const activeModel = selectableModels[activeModelIndex];
  const reasoningLabel =
    reasoningEffort ?? activeModel?.modelThinkingMode ?? null;
  return (
    <DropdownMenu
      open={activePanel === "model" || activePanel === "reasoning"}
      onOpenChange={(open) => onPanelChange(open ? "model" : null)}
    >
      <DropdownMenuTrigger
        render={
          <Button
            variant="ghost"
            className="composerModelTrigger"
            disabled={!selectableModels.length}
            aria-label={t("composerRuntimeControls.globalModels")}
          />
        }
      >
        <span className="composerModelName">
          {activeModel?.displayName ||
            activeModel?.model ||
            t("composerRuntimeControls.noModelConfigured")}
        </span>
        {reasoningLabel ? (
          <span className="composerModelEffort">
            {effortLabel(reasoningLabel)}
          </span>
        ) : null}
        <ChevronDown aria-hidden="true" />
      </DropdownMenuTrigger>
      <DropdownMenuContent
        side="top"
        align="end"
        sideOffset={8}
        collisionPadding={{ top: 8, left: 8, right: 152, bottom: 8 }}
        collisionAvoidance={{ side: "none", align: "shift" }}
        className="composerModelMenu"
      >
        <DropdownMenuRadioGroup
          value={activeModel ? modelKey(activeModel) : ""}
          onValueChange={(value) => {
            const model = selectableModels.find(
              (item) => modelKey(item) === value,
            );
            if (model) onModelSelect(model);
          }}
        >
          {selectableModels.map((model) => (
            <DropdownMenuRadioItem
              key={modelKey(model)}
              value={modelKey(model)}
              className="composerModelOption"
              aria-label={model.displayName || model.model}
            >
              <span>
                <span className="composerModelOptionName">
                  {model.displayName || model.model}
                </span>
                <small>{model.providerName}</small>
              </span>
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
        {reasoningEfforts.length ? (
          <>
            <DropdownMenuSeparator className="composerModelSeparator" />
            <DropdownMenuSub
              key={activePanel === "reasoning" ? "reasoning" : "model"}
              defaultOpen={activePanel === "reasoning"}
            >
              <DropdownMenuSubTrigger
                className="composerEffortTrigger"
                aria-label={t("composerRuntimeControls.reasoningEffort")}
              >
                <span>Effort</span>
                <span className="composerEffortValue">
                  {reasoningLabel ? effortLabel(reasoningLabel) : ""}
                </span>
              </DropdownMenuSubTrigger>
              <DropdownMenuSubContent
                aria-label={t("composerRuntimeControls.reasoningEffort")}
                side="right"
                align="end"
                alignOffset={0}
                sideOffset={6}
                collisionPadding={8}
                collisionAvoidance={{ side: "none", align: "shift" }}
                className="composerEffortMenu"
              >
                <DropdownMenuRadioGroup
                  value={reasoningLabel ?? ""}
                  onValueChange={(value) => {
                    if (reasoningEfforts.includes(value as ModelThinkingMode))
                      onReasoningEffortSelect(value as ModelThinkingMode);
                  }}
                >
                  {reasoningEfforts.map((effort) => (
                    <DropdownMenuRadioItem key={effort} value={effort}>
                      {effortLabel(effort)}
                    </DropdownMenuRadioItem>
                  ))}
                </DropdownMenuRadioGroup>
              </DropdownMenuSubContent>
            </DropdownMenuSub>
          </>
        ) : null}
      </DropdownMenuContent>
    </DropdownMenu>
  );
});

export const ComposerContextControl = memo(function ComposerContextControl({
  activePanel,
  contextUsage,
  onPanelChange,
}: Pick<
  ComposerRuntimeControlsProps,
  "activePanel" | "contextUsage" | "onPanelChange"
>) {
  const breakdown = contextUsage?.breakdown;
  const maxContextTokens = contextUsage?.maxContextTokens ?? 0;
  const usedTokens = contextUsage?.usedTokens ?? 0;
  const usedPercentage = contextUsage?.usedPercentage ?? 0;
  const contextSegments = breakdown
    ? ([
        ["Messages", breakdown.messageTokens, "messages"],
        ["System tools", breakdown.systemToolTokens, "system-tools"],
        ["MCP tools", breakdown.mcpToolTokens, "mcp-tools"],
        ["System prompt", breakdown.systemPromptTokens, "system-prompt"],
        ["Skills", breakdown.skillsTokens, "skills"],
        ["buffer", breakdown.autoCompactBufferTokens, "buffer"],
        ["Free space", breakdown.freeSpaceTokens, "free"],
      ] as const)
    : [];
  return (
    <Popover
      open={activePanel === "context"}
      onOpenChange={(open) => onPanelChange(open ? "context" : null)}
    >
      <PopoverTrigger
        className="contextWindowIndicator"
        aria-label="Context window"
        aria-expanded={activePanel === "context"}
      >
        <span
          style={
            { "--context-used": `${usedPercentage * 3.6}deg` } as CSSProperties
          }
        />
      </PopoverTrigger>
      <PopoverContent
        side="top"
        align="end"
        sideOffset={8}
        className="contextWindowPanel"
        aria-label="Context window"
      >
        <header>
          <span>Context window</span>
          {contextUsage ? (
            <strong
              title={`${formatTokenCount(usedTokens)} / ${formatTokenCount(maxContextTokens)}`}
            >
              {formatContextTokenCount(usedTokens)} /{" "}
              {formatContextTokenCount(maxContextTokens)}
            </strong>
          ) : null}
        </header>
        {!contextUsage ? (
          <p className="desktopContextEmpty">No usage yet.</p>
        ) : null}
        {breakdown ? (
          <>
            <div className="contextWindowBar">
              {contextSegments
                .filter(([, tokens]) => tokens > 0)
                .map(([label, tokens, kind]) => (
                  <i
                    key={label}
                    className={`is-${kind}`}
                    style={{
                      width: `${maxContextTokens ? (tokens / maxContextTokens) * 100 : 0}%`,
                    }}
                  />
                ))}
            </div>
            <div className="contextWindowRows">
              {contextSegments
                .filter(([, , kind]) => kind !== "buffer")
                .map(([label, tokens, kind]) => (
                  <div key={label}>
                    <i className={`is-${kind}`} />
                    <span>{label}</span>
                    <strong title={formatTokenCount(tokens)}>
                      {formatContextTokenCount(tokens)}
                    </strong>
                  </div>
                ))}
            </div>
            {breakdown.mcpTools.length ? (
              <details className="contextMcpTools">
                <summary>
                  MCP tools
                  <span>
                    {formatContextTokenCount(breakdown.mcpToolTokens)}
                  </span>
                </summary>
                <div>
                  {breakdown.mcpTools.map((tool) => (
                    <p key={`${tool.providerId}:${tool.name}`}>
                      <span title={`${tool.providerId} · ${tool.name}`}>
                        {tool.providerId} · {tool.name}
                      </span>
                      <strong title={formatTokenCount(tool.tokens)}>
                        {formatContextTokenCount(tool.tokens)}
                      </strong>
                    </p>
                  ))}
                </div>
              </details>
            ) : null}
          </>
        ) : null}
      </PopoverContent>
    </Popover>
  );
});
