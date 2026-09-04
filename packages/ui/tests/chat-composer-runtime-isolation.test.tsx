import { useState, type ChangeEvent, type ComponentProps } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, test, vi } from "vitest";
import type { ComposerRuntimeControlsProps } from "../src/components/chat/ComposerRuntimeControls";

const runtimeHarness = vi.hoisted(() => ({
  receivedProps: [] as ComposerRuntimeControlsProps[],
}));

vi.mock("../src/components/chat/ComposerRuntimeControls", () => ({
  ComposerRuntimeControls: (props: ComposerRuntimeControlsProps) => {
    runtimeHarness.receivedProps.push(props);
    return <div data-testid="runtime-controls" />;
  },
}));

import { ChatComposer } from "../src/components/chat/ChatComposer";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const EMPTY_MODELS: ComponentProps<typeof ChatComposer>["selectableModels"] = [];
const EMPTY_REASONING: ComponentProps<typeof ChatComposer>["reasoningEfforts"] = [];

test("prompt edits preserve the runtime control prop identities passed by the parent", async () => {
  runtimeHarness.receivedProps.length = 0;
  const stableProps = {
    panelResetKey: "session-one",
    isStreaming: false,
    modelRuntimeSummary: "No model",
    selectableModels: EMPTY_MODELS,
    activeModelIndex: -1,
    runtimeConfigError: "",
    reasoningEffort: null,
    reasoningEfforts: EMPTY_REASONING,
    contextUsage: null,
    compactInteractive: false,
    isCompacting: false,
    onSubmit: vi.fn(),
    onComposerAction: vi.fn(),
    onModelSelect: vi.fn(),
    onReasoningEffortSelect: vi.fn(),
    onCompact: vi.fn(),
  } satisfies Omit<
    ComponentProps<typeof ChatComposer>,
    "inputValue" | "hasInput" | "onInputChange"
  >;

  function Harness() {
    const [inputValue, setInputValue] = useState("");
    return (
      <ChatComposer
        {...stableProps}
        inputValue={inputValue}
        hasInput={Boolean(inputValue.trim())}
        onInputChange={setInputValue}
      />
    );
  }

  const rendered = { current: null as ReactTestRenderer | null };
  await act(async () => {
    rendered.current = create(<Harness />);
  });
  const renderer = rendered.current;
  if (!renderer) {
    throw new Error("Composer did not render");
  }
  expect(runtimeHarness.receivedProps).toHaveLength(1);

  const textareaProps = renderer.root.findByType("textarea").props as unknown as {
    onChange: (event: ChangeEvent<HTMLTextAreaElement>) => void;
  };
  await act(async () => {
    textareaProps.onChange({
      target: { value: "hello" },
    } as unknown as ChangeEvent<HTMLTextAreaElement>);
  });
  expect(runtimeHarness.receivedProps).toHaveLength(2);
  const [initialProps, updatedProps] = runtimeHarness.receivedProps;
  for (const key of Object.keys(initialProps) as Array<keyof ComposerRuntimeControlsProps>) {
    expect(Object.is(updatedProps[key], initialProps[key]), `${key} changed identity`).toBe(true);
  }

  await act(async () => renderer.unmount());
});
