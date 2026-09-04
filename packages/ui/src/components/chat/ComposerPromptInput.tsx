import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useRef,
  type KeyboardEvent,
} from "react";
import { ArrowUp, Square } from "lucide-react";
import { Button } from "../ui/button";
import { Tooltip } from "../ui/tooltip";

const COMPOSITION_END_ENTER_GRACE_MS = 100;

export type ComposerPromptInputHandle = {
  focus: () => void;
};

type ComposerPromptInputProps = {
  value: string;
  isStreaming: boolean;
  hasInput: boolean;
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
>(function ComposerPromptInput({
  value,
  isStreaming,
  hasInput,
  commandsExpanded,
  mcpExpanded,
  onChange,
  onSubmit,
  onAction,
  onFocus,
  onPanelKeyDown,
}, forwardedRef) {
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
    if (onPanelKeyDown(event)) {
      return;
    }
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      onSubmit();
    }
  };

  return (
    <div className="text-input-container">
      <textarea
        ref={textareaRef}
        id="message-input"
        value={value}
        placeholder={isStreaming ? "追加补充，不中断当前执行" : "输入消息…"}
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
      </Tooltip>
    </div>
  );
});
