import type { ReactNode } from "react";
import type { ReactTestInstance } from "react-test-renderer";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, test, vi } from "vitest";
import type { AgentRuntimeConfig } from "../src/lib/chatBridge";

const runtime = vi.hoisted(() => ({
  getAgentRuntimeConfig: vi.fn(),
  testAgentRuntimeModel: vi.fn(),
}));

vi.mock("../src/lib/chatBridge", () => ({
  getAgentRuntimeConfig: runtime.getAgentRuntimeConfig,
  resetAgentRuntimeConfig: vi.fn(),
  setAgentRuntimeConfig: vi.fn(),
  testAgentRuntimeModel: runtime.testAgentRuntimeModel,
}));

import { ModelsDialog } from "../src/components/ModelsDialog";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const config = {
  executionHost: "localUser",
  autoContinueAfterResumeWait: false,
  modelProviders: [{
    providerId: "custom.test",
    name: "Test provider",
    builtIn: false,
    accessKind: "custom",
    configured: true,
    models: [
      { providerId: "custom.test", providerName: "Test provider", model: "model-one", modelThinkingModes: [], supportsVision: false, builtIn: false },
      { providerId: "custom.test", providerName: "Test provider", model: "model-two", modelThinkingModes: [], supportsVision: false, builtIn: false },
    ],
  }],
  selectableModels: [],
  customModelProviders: [{
    providerId: "custom.test",
    name: "Test provider",
    baseUrl: "https://example.test/v1",
    api: "openai-responses",
    models: [
      { model: "model-one", contextTokens: "128k", maxOutputTokens: "32k", supportsVision: false },
      { model: "model-two", contextTokens: "128k", maxOutputTokens: "32k", supportsVision: false },
    ],
  }],
  updatedAt: 1,
} satisfies AgentRuntimeConfig;

const text = (children: ReactNode): string => Array.isArray(children)
  ? children.map(text).join("")
  : typeof children === "string" || typeof children === "number"
    ? String(children)
    : "";

const button = (renderer: ReactTestRenderer, label: string): ReactTestInstance =>
  renderer.root.findAllByType("button").find((candidate) => text(candidate.props.children) === label)
  ?? (() => { throw new Error(`Missing button ${label}`); })();

test("a model test result is cleared when the selected model changes", async () => {
  runtime.getAgentRuntimeConfig.mockResolvedValue(config);
  runtime.testAgentRuntimeModel.mockResolvedValue({
    httpStatus: 200,
    latencyMs: 12,
    outputPreview: "OK",
  });
  const rendered = { current: null as ReactTestRenderer | null };

  await act(async () => {
    rendered.current = create(
      <ModelsDialog
        onClose={() => {}}
        confirmAction={async () => true}
      />,
    );
    await Promise.resolve();
  });
  const renderer = rendered.current;
  if (!renderer) throw new Error("Models dialog did not render");

  await act(async () => button(renderer!, "model-one").props.onClick());
  await act(async () => {
    button(renderer!, "Test").props.onClick();
    await Promise.resolve();
  });
  expect(renderer.root.findAllByProps({ className: "modelsTestSummary is-success" })).toHaveLength(1);

  await act(async () => button(renderer!, "model-two").props.onClick());
  expect(renderer.root.findAllByProps({ className: "modelsTestSummary is-success" })).toHaveLength(0);

  await act(async () => renderer!.unmount());
});
