import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, test, vi } from "vitest";
import {
  ComposerRuntimeControls,
  type ComposerRuntimeControlsProps,
} from "../src/components/chat/ComposerRuntimeControls";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

test("reasoning controls show concrete effort and never invent a provider default", async () => {
  const props: ComposerRuntimeControlsProps = {
    activePanel: null,
    modelRuntimeSummary: "Go",
    selectableModels: [],
    activeModelIndex: -1,
    reasoningEffort: null,
    reasoningEfforts: [],
    contextUsage: null,
    runtimeConfigError: "",
    onTogglePanel: vi.fn(),
    onModelSelect: vi.fn(),
    onReasoningEffortSelect: vi.fn(),
  };
  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(<ComposerRuntimeControls {...props} />);
  });
  expect(JSON.stringify(renderer.toJSON())).not.toContain("provider default");
  expect(
    renderer.root.findAll((node) => node.props.className === "composer-chip reasoning-chip"),
  ).toHaveLength(0);
  await act(async () => {
    renderer.update(
      <ComposerRuntimeControls
        {...props}
        activePanel="reasoning"
        reasoningEffort="high"
        reasoningEfforts={["low", "high", "max"]}
      />,
    );
  });
  const options = renderer.root.findAllByProps({ role: "option" });
  expect(options.map((option) => option.props["aria-selected"])).toEqual([false, true, false]);
  await act(async () => {
    options[2].props.onClick();
  });
  expect(props.onReasoningEffortSelect).toHaveBeenCalledWith("max");
  await act(async () => {
    renderer.unmount();
  });
});
