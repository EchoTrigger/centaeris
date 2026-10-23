import { t } from "../../i18n";
import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useRef,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import { ArrowUp, Square } from "lucide-react";
import { Button } from "../ui/button";

const COMPOSITION_END_ENTER_GRACE_MS = 100;

export type ComposerPromptInputHandle = {
  focus: () => void;
};

type ComposerPromptInputProps = {
  value: string;
  leadingControls?: ReactNode;
  trailingControls?: ReactNode;
  isStreaming: boolean;
  hasInput: boolean;
  submitDisabled?: boolean;
  commandsExpanded: boolean;
  mcpExpanded: boolean;
  onChange: (value: string) => void;
  onSubmit: () => void;
  onAction: () => void;
  onFocus: () => void;
  onPanelKeyDown: (event: KeyboardEvent<HTMLTextAreaElement>) => boolean;
};

export const ComposerPromptInput = forwardRef<
  ComposerPromptInputHandle,
  ComposerPromptInputProps
>(function ComposerPromptInput(
  {
    value,
    leadingControls,
    trailingControls,
    isStreaming,
    hasInput,
    submitDisabled = false,
    commandsExpanded,
    mcpExpanded,
    onChange,
    onSubmit,
    onAction,
    onFocus,
    onPanelKeyDown,
  },
  forwardedRef,
) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const isComposingRef = useRef(false);
  const lastCompositionEndAtRef = useRef(0);

  const focus = useCallback(() => {
    requestAnimationFrame(() => textareaRef.current?.focus());
  }, []);

  useImperativeHandle(forwardedRef, () => ({ focus }), [focus]);

  useEffect(() => {
    const textarea = textareaRef.current;
    if (!textarea) {
      return;
    }
    textarea.style.height = "auto";
    if (value) {
      textarea.style.height = `${Math.min(textarea.scrollHeight, 200)}px`;
    }
  }, [value]);

  const handleKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    const nativeEvent = event.nativeEvent;
    const recentlyComposed =
      Date.now() - lastCompositionEndAtRef.current <
      COMPOSITION_END_ENTER_GRACE_MS;
    const isComposing =
      isComposingRef.current ||
      nativeEvent.isComposing ||
      nativeEvent.keyCode === 229;

    if (
      event.key === "Enter" &&
      !event.shiftKey &&
      (isComposing || recentlyComposed)
    ) {
      if (recentlyComposed) {
        event.preventDefault();
      }
      return;
    }
    if (onPanelKeyDown(event)) {
      return;
    }
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      if (!submitDisabled) onSubmit();
    }
  };

  return (
    <div className="text-input-container">
      <textarea
        ref={textareaRef}
        id="message-input"
        aria-label="Message"
        value={value}
        placeholder={
          isStreaming
            ? t("composerPromptInput.addInputWithoutInterruptingTheCurrentRun")
            : t("composerPromptInput.message")
        }
        rows={1}
        aria-controls={commandsExpanded ? "composer-slash-commands" : undefined}
        aria-expanded={commandsExpanded || mcpExpanded}
        onChange={(event) => onChange(event.target.value)}
        onFocus={onFocus}
        onKeyDown={handleKeyDown}
        onCompositionStart={() => {
          isComposingRef.current = true;
        }}
        onCompositionEnd={() => {
          isComposingRef.current = false;
          lastCompositionEndAtRef.current = Date.now();
        }}
      />
      <div className="composerToolbar">
        <div className="composerToolbarLeading">{leadingControls}</div>
        <div className="composerToolbarTrailing">
          {trailingControls}
          <Button
            type="button"
            variant={!hasInput && isStreaming ? "composerStop" : "composerSend"}
            size="composerSend"
            className={`send-button ${!hasInput && isStreaming ? "is-stop" : ""}`}
            disabled={
              (!hasInput && !isStreaming) ||
              (submitDisabled && (hasInput || !isStreaming))
            }
            aria-label={
              !hasInput && isStreaming
                ? t("composerPromptInput.stopThisConversation")
                : t("composerPromptInput.send")
            }
            onClick={() => {
              onAction();
              focus();
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
        </div>
      </div>
    </div>
  );
});
