import { memo, useCallback, useEffect, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import type {
  AgentContextUsageSummary,
  ModelThinkingMode,
  SelectableModel,
} from "../../lib/chatBridge";
import {
  McpComposerPanels,
  useMcpComposerController,
  type McpComposerMode,
} from "./McpComposerPanel";
import {
  ComposerPromptInput,
  type ComposerPromptInputHandle,
} from "./ComposerPromptInput";
import {
  ComposerRuntimeControls,
  type RuntimeComposerPanel,
} from "./ComposerRuntimeControls";

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

const slashCommandQuery = (value: string): string | null =>
  /^\/[^\s]*$/.test(value) ? value.slice(1).toLowerCase() : null;

const ChatComposerLifecycle = memo(function ChatComposerLifecycle({
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
  const promptInputRef = useRef<ComposerPromptInputHandle>(null);
  const [activePanel, setActivePanel] = useState<ComposerPanel>(null);
  const [selectedCommandIndex, setSelectedCommandIndex] = useState(0);
  const focusTextarea = useCallback(() => {
    promptInputRef.current?.focus();
  }, []);
  const handleMcpModeChange = useCallback((mode: McpComposerMode) => {
    setActivePanel(mode);
  }, []);
  const mcpController = useMcpComposerController({
    mode: activePanel === "mcp" || activePanel === "mcp-configure" ? activePanel : null,
    panelResetKey,
    onModeChange: handleMcpModeChange,
    focusTextarea,
  });
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

  const handleRuntimePanelToggle = useCallback((panel: Exclude<RuntimeComposerPanel, null>) => {
    setActivePanel((current) => current === panel ? null : panel);
  }, []);

  const handleRuntimeModelSelect = useCallback((model: SelectableModel) => {
    onModelSelect(model);
    setActivePanel(null);
  }, [onModelSelect]);

  const handleRuntimeReasoningEffortSelect = useCallback((effort: ModelThinkingMode) => {
    onReasoningEffortSelect(effort);
    setActivePanel(null);
  }, [onReasoningEffortSelect]);

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
        mcpController.openCatalog();
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

  const handlePromptPanelKeyDown = (
    event: ReactKeyboardEvent<HTMLTextAreaElement>,
  ): boolean => {
    if (activePanel === "commands") {
      if (event.key === "ArrowDown") {
        event.preventDefault();
        moveCommandSelection(1);
        return true;
      }
      if (event.key === "ArrowUp") {
        event.preventDefault();
        moveCommandSelection(-1);
        return true;
      }
      if (event.key === "Tab") {
        event.preventDefault();
        moveCommandSelection(event.shiftKey ? -1 : 1);
        return true;
      }
      if (event.key === "Enter" && !event.shiftKey) {
        event.preventDefault();
        if (selectedCommand) {
          runComposerCommand(selectedCommand);
        }
        return true;
      }
    }
    if (mcpController.handleKeyDown(event)) {
      return true;
    }
    if (event.key === "Escape" && activePanel) {
      event.preventDefault();
      setActivePanel(null);
      return true;
    }
    return false;
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
        <McpComposerPanels controller={mcpController} />
        <ComposerPromptInput
          ref={promptInputRef}
          value={inputValue}
          isStreaming={isStreaming}
          hasInput={hasInput}
          commandsExpanded={activePanel === "commands"}
          mcpExpanded={activePanel === "mcp"}
          onChange={onInputChange}
          onSubmit={() => {
            setActivePanel(null);
            onSubmit();
          }}
          onAction={() => {
            setActivePanel(null);
            onComposerAction();
          }}
          onFocus={() => {
            if (commandQuery !== null) {
              setActivePanel("commands");
            }
          }}
          onPanelKeyDown={handlePromptPanelKeyDown}
        />
      </div>
      <ComposerRuntimeControls
        activePanel={
          activePanel === "model" || activePanel === "reasoning" || activePanel === "context"
            ? activePanel
            : null
        }
        modelRuntimeSummary={modelRuntimeSummary}
        selectableModels={selectableModels}
        activeModelIndex={activeModelIndex}
        reasoningEffort={reasoningEffort}
        reasoningEfforts={reasoningEfforts}
        contextUsage={contextUsage}
        runtimeConfigError={runtimeConfigError}
        onTogglePanel={handleRuntimePanelToggle}
        onModelSelect={handleRuntimeModelSelect}
        onReasoningEffortSelect={handleRuntimeReasoningEffortSelect}
      />
    </div>
  );
});

export const ChatComposer = memo(function ChatComposer(props: ChatComposerProps) {
  return <ChatComposerLifecycle {...props} key={props.panelResetKey} />;
});
