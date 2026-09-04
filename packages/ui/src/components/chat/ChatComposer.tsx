import { memo, useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { ArrowLeft, ArrowUp, Check, ChevronDown, Image, LockKeyhole, Square } from "lucide-react";
import { Button } from "../ui/button";
import { Tooltip } from "../ui/tooltip";
import type {
  AgentContextUsageSummary,
  ModelThinkingMode,
  NativeMcpCatalog,
  NativeMcpServer,
  SelectableModel,
} from "../../lib/chatBridge";
import { configureNativeMcp, getNativeMcpCatalog } from "../../lib/chatBridge";
import { formatTokenCount } from "./chatRuntimeCore";

type ResourceKind = "models" | "skills" | "plugins";
type ComposerPanel = "commands" | "model" | "reasoning" | "context" | "mcp" | "mcp-configure" | null;
type ComposerCommandId =
  | "new"
  | "model"
  | "effort"
  | "state"
  | "compact"
  | "mcp"
  | ResourceKind;

type ComposerCommand = {
  id: ComposerCommandId;
  name: string;
  description: string;
  disabled?: boolean;
};

type ChatComposerProps = {
  panelResetKey: string;
  inputValue: string;
  isStreaming: boolean;
  hasInput: boolean;
  modelRuntimeSummary: string;
  selectableModels: SelectableModel[];
  activeModelIndex: number;
  runtimeConfigError: string;
  reasoningEffort: ModelThinkingMode | null;
  reasoningEfforts: ModelThinkingMode[];
  contextUsage: AgentContextUsageSummary | null;
  compactInteractive: boolean;
  isCompacting: boolean;
  onInputChange: (value: string) => void;
  onSubmit: () => void;
  onComposerAction: () => void;
  onModelSelect: (model: SelectableModel) => void;
  onReasoningEffortSelect: (effort: ModelThinkingMode) => void;
  onCompact: () => void;
  onNewSession?: () => void;
  onOpenResource?: (resource: ResourceKind) => void;
};

const COMPOSITION_END_ENTER_GRACE_MS = 100;

const formatContextTokenCount = (value: number): string =>
  new Intl.NumberFormat("en-US", {
    notation: "compact",
    maximumFractionDigits: 1,
  }).format(value).toLowerCase();

const slashCommandQuery = (value: string): string | null =>
  /^\/[^\s]*$/.test(value) ? value.slice(1).toLowerCase() : null;

export const ChatComposer = memo(function ChatComposer({
  panelResetKey,
  inputValue,
  isStreaming,
  hasInput,
  modelRuntimeSummary,
  selectableModels,
  activeModelIndex,
  runtimeConfigError,
  reasoningEffort,
  reasoningEfforts,
  contextUsage,
  compactInteractive,
  isCompacting,
  onInputChange,
  onSubmit,
  onComposerAction,
  onModelSelect,
  onReasoningEffortSelect,
  onCompact,
  onNewSession,
  onOpenResource,
}: ChatComposerProps) {
  const composerRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const mcpTokenRef = useRef<HTMLInputElement>(null);
  const isComposingRef = useRef(false);
  const lastCompositionEndAtRef = useRef(0);
  const [activePanel, setActivePanel] = useState<ComposerPanel>(null);
  const [selectedCommandIndex, setSelectedCommandIndex] = useState(0);
  const [mcpCatalog, setMcpCatalog] = useState<NativeMcpCatalog | null>(null);
  const [mcpLoading, setMcpLoading] = useState(false);
  const [mcpError, setMcpError] = useState("");
  const [mcpNotice, setMcpNotice] = useState("");
  const [selectedMcpIndex, setSelectedMcpIndex] = useState(0);
  const [configuringMcpServer, setConfiguringMcpServer] = useState<NativeMcpServer | null>(null);
  const [mcpBearerToken, setMcpBearerToken] = useState("");
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
  const commands: ComposerCommand[] = [
    {
      id: "new",
      name: "/new",
      description: "Start a new session",
      disabled: !onNewSession,
    },
    {
      id: "model",
      name: "/model",
      description: "Switch the active model",
      disabled: selectableModels.length === 0,
    },
    {
      id: "effort",
      name: "/effort",
      description: "Set reasoning effort",
      disabled: reasoningEfforts.length === 0,
    },
    {
      id: "state",
      name: "/state",
      description: "Show context usage",
    },
    {
      id: "compact",
      name: "/compact",
      description: isCompacting ? "Compaction in progress" : "Compact current conversation",
      disabled: !compactInteractive || isStreaming || isCompacting,
    },
    {
      id: "models",
      name: "/models",
      description: "Manage model providers",
      disabled: !onOpenResource,
    },
    {
      id: "skills",
      name: "/skills",
      description: "Manage Skills",
      disabled: !onOpenResource,
    },
    {
      id: "plugins",
      name: "/plugins",
      description: "Manage Plugins",
      disabled: !onOpenResource,
    },
    {
      id: "mcp",
      name: "/mcp",
      description: "View and configure MCP servers",
    },
  ];
  const commandQuery = slashCommandQuery(inputValue);
  const matchingCommands = commandQuery === null
    ? []
    : commands.filter((command) =>
      command.name.slice(1).startsWith(commandQuery)
    );
  const selectedCommand = matchingCommands[
    Math.min(selectedCommandIndex, Math.max(matchingCommands.length - 1, 0))
  ];

  const loadMcpCatalog = async () => {
    setMcpLoading(true);
    setMcpError("");
    try {
      setMcpCatalog(await getNativeMcpCatalog());
      setSelectedMcpIndex(0);
    } catch (error) {
      setMcpError(error instanceof Error ? error.message : String(error));
    } finally {
      setMcpLoading(false);
    }
  };

  const openMcpConfiguration = (server: NativeMcpServer) => {
    if (!server.configurable || server.status === "disabled" || server.status === "unsupported") {
      return;
    }
    setConfiguringMcpServer(server);
    setMcpBearerToken("");
    setMcpError("");
    setActivePanel("mcp-configure");
    requestAnimationFrame(() => mcpTokenRef.current?.focus());
  };

  const saveMcpConfiguration = async () => {
    if (!configuringMcpServer || !mcpBearerToken) {
      return;
    }
    setMcpLoading(true);
    setMcpError("");
    try {
      const catalog = await configureNativeMcp({
        pluginName: configuringMcpServer.pluginName,
        serverId: configuringMcpServer.serverId,
        bearerToken: mcpBearerToken,
      });
      setMcpCatalog(catalog);
      setMcpBearerToken("");
      setMcpNotice("Saved · applies to next run");
      setActivePanel("mcp");
      focusTextarea();
    } catch (error) {
      setMcpError(error instanceof Error ? error.message : String(error));
    } finally {
      setMcpLoading(false);
    }
  };

  useEffect(() => {
    const textarea = textareaRef.current;
    if (!textarea) {
      return;
    }
    textarea.style.height = "auto";
    if (inputValue) {
      textarea.style.height = `${Math.min(textarea.scrollHeight, 200)}px`;
    }
  }, [inputValue]);

  useEffect(() => {
    setSelectedCommandIndex(0);
    setActivePanel((current) =>
      commandQuery !== null
        ? "commands"
        : current === "commands"
          ? null
          : current
    );
  }, [commandQuery]);

  useEffect(() => {
    setActivePanel(null);
    setMcpBearerToken("");
  }, [panelResetKey]);

  useEffect(() => {
    if (activePanel !== "mcp-configure") {
      setMcpBearerToken("");
    }
  }, [activePanel]);

  useEffect(() => {
    if (!activePanel) {
      return;
    }
    const closeOnOutsidePointer = (event: PointerEvent) => {
      if (
        event.target instanceof Node
        && !composerRef.current?.contains(event.target)
      ) {
        setActivePanel(null);
      }
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setMcpBearerToken("");
        setActivePanel(activePanel === "mcp-configure" ? "mcp" : null);
      }
    };
    document.addEventListener("pointerdown", closeOnOutsidePointer);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutsidePointer);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [activePanel]);

  const focusTextarea = () => {
    requestAnimationFrame(() => textareaRef.current?.focus());
  };

  const togglePanel = (panel: Exclude<ComposerPanel, "commands" | null>) => {
    setActivePanel((current) => current === panel ? null : panel);
  };

  const runComposerCommand = (command: ComposerCommand) => {
    if (command.disabled) {
      return;
    }
    onInputChange("");
    switch (command.id) {
      case "new":
        setActivePanel(null);
        onNewSession?.();
        break;
      case "model":
        setActivePanel("model");
        focusTextarea();
        break;
      case "effort":
        setActivePanel("reasoning");
        focusTextarea();
        break;
      case "state":
        setActivePanel("context");
        focusTextarea();
        break;
      case "compact":
        setActivePanel(null);
        onCompact();
        focusTextarea();
        break;
      case "mcp":
        setMcpNotice("");
        setActivePanel("mcp");
        void loadMcpCatalog();
        focusTextarea();
        break;
      case "models":
      case "skills":
      case "plugins":
        setActivePanel(null);
        onOpenResource?.(command.id);
        break;
    }
  };

  const moveCommandSelection = (delta: number) => {
    if (!matchingCommands.length) {
      return;
    }
    setSelectedCommandIndex((current) =>
      (current + delta + matchingCommands.length) % matchingCommands.length
    );
  };

  const moveMcpSelection = (delta: number) => {
    const count = mcpCatalog?.servers.length ?? 0;
    if (!count) {
      return;
    }
    setSelectedMcpIndex((current) => (current + delta + count) % count);
  };

  return (
    <div className="chatComposerRoot" ref={composerRef}>
      <div className="input-wrapper">
        {activePanel === "commands" ? (
          <div
            className="slashCommandPanel"
            id="composer-slash-commands"
            role="listbox"
            aria-label="Commands"
          >
            <header>
              <span>Commands · {matchingCommands.length}</span>
              <span>↑↓ / Tab · Enter</span>
            </header>
            <div>
              {matchingCommands.length ? matchingCommands.map((command, index) => (
                <button
                  type="button"
                  role="option"
                  aria-selected={index === selectedCommandIndex}
                  className={index === selectedCommandIndex ? "is-selected" : ""}
                  disabled={command.disabled}
                  key={command.id}
                  onMouseEnter={() => setSelectedCommandIndex(index)}
                  onClick={() => runComposerCommand(command)}
                >
                  <strong>{command.name}</strong>
                  <span>{command.description}</span>
                </button>
              )) : (
                <p>No matching command</p>
              )}
            </div>
          </div>
        ) : null}
        {activePanel === "mcp" ? (
          <section className="slashCommandPanel mcpComposerPanel" aria-label="MCP servers">
            <header>
              <strong>MCP</strong>
              <span>{mcpNotice || `${mcpCatalog?.servers.length ?? 0} servers`}</span>
            </header>
            <div className="mcpServerList" role="listbox">
              {mcpLoading && !mcpCatalog ? <p>Loading MCP servers…</p> : null}
              {mcpError ? <p className="mcpPanelError" role="status">{mcpError}</p> : null}
              {!mcpLoading && !mcpError && !mcpCatalog?.servers.length ? <p>No MCP servers</p> : null}
              {mcpCatalog?.servers.map((server, index) => {
                const actionable = server.configurable
                  && server.status !== "disabled"
                  && server.status !== "unsupported";
                const status = server.status === "needsConfiguration"
                  ? "Configure"
                  : server.status === "disabled"
                    ? "Plugin disabled"
                    : server.status === "unsupported"
                      ? "Unsupported"
                      : server.configurable
                        ? "Configured"
                        : "Managed";
                return (
                  <button
                    type="button"
                    role="option"
                    aria-selected={index === selectedMcpIndex}
                    className={`mcpServerRow ${index === selectedMcpIndex ? "is-selected" : ""} ${actionable ? "" : "is-locked"}`}
                    disabled={!actionable}
                    key={`${server.pluginName}:${server.serverId}`}
                    onMouseEnter={() => setSelectedMcpIndex(index)}
                    onClick={() => openMcpConfiguration(server)}
                  >
                    <span className="mcpServerIdentity">
                      <strong>{server.serverId}</strong>
                      <small>{server.pluginDisplayName} · {server.toolNames.length} tool{server.toolNames.length === 1 ? "" : "s"}</small>
                    </span>
                    <span className={`mcpServerStatus is-${server.status}`}>
                      {!actionable ? <LockKeyhole aria-hidden="true" /> : null}
                      {status}
                    </span>
                  </button>
                );
              })}
            </div>
          </section>
        ) : null}
        {activePanel === "mcp-configure" && configuringMcpServer ? (
          <section className="slashCommandPanel mcpComposerPanel" aria-label="Configure MCP server">
            <header>
              <button
                type="button"
                className="mcpPanelBack"
                aria-label="Back to MCP servers"
                onClick={() => {
                  setMcpBearerToken("");
                  setMcpError("");
                  setActivePanel("mcp");
                }}
              >
                <ArrowLeft aria-hidden="true" />
                MCP
              </button>
              <span>{configuringMcpServer.serverId}</span>
            </header>
            <form
              className="mcpConfigForm"
              onSubmit={(event) => {
                event.preventDefault();
                void saveMcpConfiguration();
              }}
            >
              <label>
                <span>API key</span>
                <input
                  ref={mcpTokenRef}
                  type="password"
                  autoComplete="new-password"
                  value={mcpBearerToken}
                  disabled={mcpLoading}
                  onChange={(event) => setMcpBearerToken(event.target.value)}
                />
              </label>
              <p className="mcpEndpoint" title={configuringMcpServer.endpoint ?? ""}>
                {configuringMcpServer.endpoint}
              </p>
              {mcpError ? <p className="mcpPanelError" role="status">{mcpError}</p> : null}
              <div className="mcpConfigActions">
                <button
                  type="button"
                  onClick={() => {
                    setMcpBearerToken("");
                    setMcpError("");
                    setActivePanel("mcp");
                  }}
                >
                  Cancel
                </button>
                <button type="submit" className="is-primary" disabled={!mcpBearerToken || mcpLoading}>
                  {mcpLoading ? "Testing…" : "Save & test"}
                </button>
              </div>
            </form>
          </section>
        ) : null}
        <div className="text-input-container">
          <textarea
            ref={textareaRef}
            id="message-input"
            value={inputValue}
            placeholder={
              isStreaming
                ? "追加补充，不中断当前执行"
                : "输入消息…"
            }
            rows={1}
            aria-controls={activePanel === "commands" ? "composer-slash-commands" : undefined}
            aria-expanded={activePanel === "commands" || activePanel === "mcp"}
            onChange={(event) => onInputChange(event.target.value)}
            onFocus={() => {
              if (commandQuery !== null) {
                setActivePanel("commands");
              }
            }}
            onKeyDown={(event) => {
              const nativeEvent = event.nativeEvent;
              const recentlyComposed =
                Date.now() - lastCompositionEndAtRef.current
                < COMPOSITION_END_ENTER_GRACE_MS;
              const isComposing =
                isComposingRef.current
                || nativeEvent.isComposing
                || nativeEvent.keyCode === 229;

              if (
                event.key === "Enter"
                && !event.shiftKey
                && (isComposing || recentlyComposed)
              ) {
                if (recentlyComposed) {
                  event.preventDefault();
                }
                return;
              }
              if (activePanel === "commands") {
                if (event.key === "ArrowDown") {
                  event.preventDefault();
                  moveCommandSelection(1);
                  return;
                }
                if (event.key === "ArrowUp") {
                  event.preventDefault();
                  moveCommandSelection(-1);
                  return;
                }
                if (event.key === "Tab") {
                  event.preventDefault();
                  moveCommandSelection(event.shiftKey ? -1 : 1);
                  return;
                }
                if (event.key === "Enter" && !event.shiftKey) {
                  event.preventDefault();
                  if (selectedCommand) {
                    runComposerCommand(selectedCommand);
                  }
                  return;
                }
              }
              if (activePanel === "mcp") {
                if (event.key === "ArrowDown") {
                  event.preventDefault();
                  moveMcpSelection(1);
                  return;
                }
                if (event.key === "ArrowUp") {
                  event.preventDefault();
                  moveMcpSelection(-1);
                  return;
                }
                if (event.key === "Enter" && !event.shiftKey) {
                  event.preventDefault();
                  const server = mcpCatalog?.servers[selectedMcpIndex];
                  if (server) {
                    openMcpConfiguration(server);
                  }
                  return;
                }
              }
              if (event.key === "Escape" && activePanel) {
                event.preventDefault();
                setActivePanel(null);
                return;
              }
              if (event.key === "Enter" && !event.shiftKey) {
                event.preventDefault();
                setActivePanel(null);
                onSubmit();
              }
            }}
            onCompositionStart={() => {
              isComposingRef.current = true;
            }}
            onCompositionEnd={() => {
              isComposingRef.current = false;
              lastCompositionEndAtRef.current = Date.now();
            }}
          />
          <Tooltip
            align="end"
            content={!hasInput && isStreaming ? "停止当前会话" : "发送"}
          >
            <Button
              type="button"
              variant={!hasInput && isStreaming ? "composerStop" : "composerSend"}
              size="composerSend"
              className={`send-button ${!hasInput && isStreaming ? "is-stop" : ""}`}
              disabled={!hasInput && !isStreaming}
              aria-label={!hasInput && isStreaming ? "停止当前会话" : "发送"}
              onClick={() => {
                setActivePanel(null);
                onComposerAction();
                focusTextarea();
              }}
            >
              {!hasInput && isStreaming ? (
                <Square
                  className="composerLucideIcon action-icon"
                  aria-hidden="true"
                />
              ) : (
                <ArrowUp
                  className="composerLucideIcon action-icon"
                  aria-hidden="true"
                />
              )}
            </Button>
          </Tooltip>
        </div>
      </div>
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
              onClick={() => togglePanel("model")}
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
                          onClick={() => {
                            onModelSelect(configured);
                            setActivePanel(null);
                          }}
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
                onClick={() => togglePanel("reasoning")}
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
                        onClick={() => {
                          onReasoningEffortSelect(effort);
                          setActivePanel(null);
                        }}
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
              onClick={() => togglePanel("context")}
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
    </div>
  );
});
