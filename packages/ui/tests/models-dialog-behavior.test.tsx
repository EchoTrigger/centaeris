// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { AgentRuntimeConfig } from "../src/lib/chatBridge";

const runtime = vi.hoisted(() => ({
  getAgentRuntimeConfig: vi.fn(),
  resetAgentRuntimeConfig: vi.fn(),
  testAgentRuntimeModel: vi.fn(),
  setAgentRuntimeConfig: vi.fn(),
}));
vi.mock("../src/lib/chatBridge", () => ({
  getAgentRuntimeConfig: runtime.getAgentRuntimeConfig,
  resetAgentRuntimeConfig: runtime.resetAgentRuntimeConfig,
  setAgentRuntimeConfig: runtime.setAgentRuntimeConfig,
  testAgentRuntimeModel: runtime.testAgentRuntimeModel,
}));
import { ModelsDialog } from "../src/components/ModelsDialog";

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

beforeEach(() => {
  vi.resetAllMocks();
  runtime.getAgentRuntimeConfig.mockResolvedValue(config);
  runtime.setAgentRuntimeConfig.mockResolvedValue(config);
  runtime.testAgentRuntimeModel.mockResolvedValue({ httpStatus: 200, latencyMs: 12, outputPreview: "OK" });
});
afterEach(cleanup);
function renderModels() {
  const user = userEvent.setup();
  render(<ModelsDialog onClose={() => {}} confirmAction={async () => true} />);
  return user;
}
async function openProvider(user: ReturnType<typeof userEvent.setup>, name = "Test provider") {
  await user.click(await screen.findByRole("button", { name: `Open ${name}` }));
}

test("a model test result is cleared when the selected model changes", async () => {
  const user = renderModels();
  await openProvider(user);
  await user.click(screen.getByRole("button", { name: "model-one" }));
  const first = await screen.findByRole("dialog", { name: "model-one" });
  await user.click(within(first).getByRole("button", { name: "Test" }));
  expect(await within(first).findByText("Connected · 12ms · HTTP 200 · OK")).toBeDefined();
  expect(runtime.testAgentRuntimeModel).toHaveBeenCalledExactlyOnceWith({ providerId: "custom.test", model: "model-one" });
  await user.click(within(first).getByRole("button", { name: "Close detail" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  await user.click(screen.getByRole("button", { name: "model-two" }));
  const second = await screen.findByRole("dialog", { name: "model-two" });
  expect(within(second).queryByText("Connected · 12ms · HTTP 200 · OK")).toBeNull();
  expect(within(second).getByRole("button", { name: "Test" })).toBeDefined();
});

test("service landing has no global save and cancelling a connection edit discards its draft", async () => {
  const user = renderModels();
  await screen.findByRole("button", { name: "Open Test provider" });
  expect(screen.queryByRole("button", { name: "Save" })).toBeNull();
  await openProvider(user);
  await user.click(screen.getByRole("button", { name: "Edit connection" }));
  const name = screen.getByRole("textbox", { name: "Provider name" });
  await user.clear(name);
  await user.type(name, "Changed");
  await user.click(screen.getByRole("button", { name: "Cancel" }));
  await user.click(screen.getByRole("button", { name: "Edit connection" }));
  expect((screen.getByRole("textbox", { name: "Provider name" }) as HTMLInputElement).value).toBe("Test provider");
});

test("connection save reports partial failure, retains the key for retry, and keeps the edit open", async () => {
  runtime.setAgentRuntimeConfig.mockResolvedValueOnce(config).mockRejectedValueOnce(new Error("credential store unavailable"));
  const user = renderModels();
  await openProvider(user);
  await user.click(screen.getByRole("button", { name: "Edit connection" }));
  await user.type(screen.getByLabelText("API key", { exact: true }), "test-key");
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect((await screen.findByRole("status")).textContent).toContain("API key was not saved");
  expect(runtime.setAgentRuntimeConfig.mock.calls[0][0]).toHaveProperty("customModelProviders");
  expect(runtime.setAgentRuntimeConfig.mock.calls[1][0]).toEqual({ modelProviderId: "custom.test", modelApiKey: "test-key" });
  expect((screen.getByLabelText("API key", { exact: true }) as HTMLInputElement).value).toBe("test-key");
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(screen.queryByLabelText("API key", { exact: true })).toBeNull());
  expect((await screen.findByRole("status")).textContent).toBe("Saved");
});

test("testing a model from the service list targets its provider and model", async () => {
  const user = renderModels();
  await openProvider(user);
  await user.click(screen.getByRole("button", { name: "Test model-two" }));
  expect(runtime.testAgentRuntimeModel).toHaveBeenCalledExactlyOnceWith({ providerId: "custom.test", model: "model-two" });
  expect(await screen.findByText("model-two: Connected · 12ms · HTTP 200 · OK")).toBeDefined();
  expect(screen.queryByRole("dialog")).toBeNull();
});

test("built-in services expose API credentials without custom connection fields", async () => {
  runtime.getAgentRuntimeConfig.mockResolvedValue({ ...config, customModelProviders: [], modelProviders: [{
    ...config.modelProviders[0], providerId: "builtin.test", name: "Built-in service", builtIn: true, accessKind: "api_key",
  }] });
  const user = renderModels();
  await openProvider(user, "Built-in service");
  await user.click(screen.getByRole("button", { name: "Edit connection" }));
  expect((screen.getByLabelText("API key", { exact: true }) as HTMLInputElement).type).toBe("password");
  expect(screen.queryByRole("textbox", { name: "Provider name" })).toBeNull();
  expect(screen.queryByRole("combobox")).toBeNull();
  expect(screen.queryByRole("button", { name: "Add model" })).toBeNull();
});


test("connection edits expose save only after a change", async () => {
  const user = renderModels();
  await openProvider(user);
  await user.click(screen.getByRole("button", { name: "Edit connection" }));
  expect((screen.getByRole("button", { name: "Save" }) as HTMLButtonElement).disabled).toBe(true);
  await user.type(screen.getByRole("textbox", { name: "Provider name" }), " updated");
  expect((screen.getByRole("button", { name: "Save" }) as HTMLButtonElement).disabled).toBe(false);
});

test("a test completed after leaving and reopening the model cannot restore a stale result", async () => {
  let finish!: (value: { httpStatus: number; latencyMs: number; outputPreview: string }) => void;
  runtime.testAgentRuntimeModel.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const user = renderModels();
  await openProvider(user);
  await user.click(screen.getByRole("button", { name: "model-one" }));
  await user.click(within(await screen.findByRole("dialog", { name: "model-one" })).getByRole("button", { name: "Test" }));
  await user.click(screen.getByRole("button", { name: "Close detail" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  await user.click(screen.getByRole("button", { name: "model-one" }));
  const current = await screen.findByRole("dialog", { name: "model-one" });
  await act(async () => finish({ httpStatus: 200, latencyMs: 12, outputPreview: "obsolete result" }));
  expect(within(current).queryByText(/obsolete result/)).toBeNull();
  expect((within(current).getByRole("button", { name: "Test" }) as HTMLButtonElement).disabled).toBe(false);
});

test("custom service creation, model capabilities, and protocol overrides save through the runtime contract", async () => {
  runtime.setAgentRuntimeConfig.mockImplementation(async input => {
    const providers = input.customModelProviders ?? config.customModelProviders;
    return {
      ...config,
      customModelProviders: providers,
      modelProviders: providers.map((provider: NonNullable<AgentRuntimeConfig["customModelProviders"]>[number]) => ({
        providerId: provider.providerId, name: provider.name, builtIn: false, accessKind: "custom", configured: true,
        models: provider.models.map(model => ({ ...model, providerId: provider.providerId, providerName: provider.name, modelThinkingModes: [], builtIn: false })),
      })),
    };
  });
  const user = renderModels();
  await user.click(await screen.findByRole("button", { name: "Add service" }));
  const picker = await screen.findByRole("dialog", { name: "Add service" });
  await user.click(within(picker).getByRole("button", { name: /Custom service/ }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  await user.clear(screen.getByRole("textbox", { name: "Provider name" }));
  await user.type(screen.getByRole("textbox", { name: "Provider name" }), "Local lab");
  await user.type(screen.getByRole("textbox", { name: /Base URL/ }), "http://localhost:1234/v1");
  await user.click(screen.getByRole("combobox", { name: "API protocol" }));
  await user.click(await screen.findByRole("option", { name: "anthropic-messages" }));
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect((await screen.findByRole("status")).textContent).toBe("Saved");
  const created = runtime.setAgentRuntimeConfig.mock.calls[0][0].customModelProviders[1];
  expect(created).toEqual(expect.objectContaining({ name: "Local lab", baseUrl: "http://localhost:1234/v1", api: "anthropic-messages", models: [] }));
  expect(created.providerId).toMatch(/^custom\./);
  await user.click(screen.getByRole("button", { name: "Add model" }));
  const editor = await screen.findByRole("dialog", { name: "New model" });
  await user.type(within(editor).getByRole("textbox", { name: /ID \*/ }), "vision-model");
  await user.type(within(editor).getByRole("textbox", { name: "Name" }), "Lab vision");
  await user.click(within(editor).getByRole("combobox", { name: "API override" }));
  await user.click(await screen.findByRole("option", { name: "openai-responses" }));
  await user.clear(within(editor).getByRole("textbox", { name: "Context window" }));
  await user.type(within(editor).getByRole("textbox", { name: "Context window" }), "256k");
  await user.click(within(editor).getByRole("checkbox", { name: "Image input" }));
  expect((within(editor).getByRole("button", { name: "Test" }) as HTMLButtonElement).disabled).toBe(true);
  await user.click(within(editor).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(runtime.setAgentRuntimeConfig.mock.calls[1][0].customModelProviders[1]).toEqual({
    ...created,
    models: [{ model: "vision-model", displayName: "Lab vision", contextTokens: "256k", maxOutputTokens: "32k", apiOverride: "openai-responses", supportsVision: true }],
  });
  expect(screen.getByRole("button", { name: "Lab vision" })).toBeDefined();
});

test("cancelling a new model discards its ID and capability edits without writing configuration", async () => {
  const user = renderModels();
  await openProvider(user);
  await user.click(screen.getByRole("button", { name: "Add model" }));
  const editor = await screen.findByRole("dialog", { name: "New model" });
  await user.type(within(editor).getByRole("textbox", { name: /ID \*/ }), "discard-me");
  await user.click(within(editor).getByRole("checkbox", { name: "Image input" }));
  await user.click(within(editor).getByRole("button", { name: "Cancel" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(screen.queryByRole("button", { name: "discard-me" })).toBeNull();
  expect(runtime.setAgentRuntimeConfig).not.toHaveBeenCalled();
  expect(screen.getByRole("button", { name: "model-one" })).toBeDefined();
});

test("service actions remove only the stored credential and recover after a failed write", async () => {
  const storedConfig = { ...config, modelProviders: [{ ...config.modelProviders[0], credentialSource: "stored" }] };
  runtime.getAgentRuntimeConfig.mockResolvedValue(storedConfig);
  runtime.setAgentRuntimeConfig.mockRejectedValueOnce(new Error("credential store unavailable")).mockResolvedValueOnce(config);
  const user = renderModels();
  await openProvider(user);
  await user.click(screen.getByRole("button", { name: "Service actions" }));
  await user.click(await screen.findByRole("menuitem", { name: "Remove credential" }));
  expect((await screen.findByRole("status")).textContent).toBe("credential store unavailable");
  expect(runtime.setAgentRuntimeConfig).toHaveBeenCalledExactlyOnceWith({ modelProviderId: "custom.test", clearModelApiKey: true });
  await user.click(screen.getByRole("button", { name: "Service actions" }));
  await user.click(await screen.findByRole("menuitem", { name: "Remove credential" }));
  expect((await screen.findByRole("status")).textContent).toBe("Credential removed");
  expect(runtime.setAgentRuntimeConfig).toHaveBeenCalledTimes(2);
});

test("a failed configuration load offers an explicit retry without discarding the error", async () => {
  runtime.getAgentRuntimeConfig.mockRejectedValueOnce(new Error("runtime unavailable")).mockResolvedValueOnce(config);
  const user = renderModels();
  expect((await screen.findByText("runtime unavailable")).closest("[role=status]")).not.toBeNull();
  await user.click(screen.getByRole("button", { name: "Retry" }));
  expect(await screen.findByRole("button", { name: "Open Test provider" })).toBeDefined();
  expect(runtime.getAgentRuntimeConfig).toHaveBeenCalledTimes(2);
  expect(screen.queryByText("runtime unavailable")).toBeNull();
});

test("removing a model locks repeated writes and closing until the runtime responds", async () => {
  let finish!: (value: AgentRuntimeConfig) => void;
  runtime.setAgentRuntimeConfig.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const user = renderModels();
  await openProvider(user);
  await user.click(screen.getByRole("button", { name: "model-one" }));
  const editor = await screen.findByRole("dialog", { name: "model-one" });
  await user.click(within(editor).getByRole("button", { name: "Remove model" }));
  await waitFor(() => expect(runtime.setAgentRuntimeConfig).toHaveBeenCalledTimes(1));
  expect((within(editor).getByRole("button", { name: "Remove model" }) as HTMLButtonElement).disabled).toBe(true);
  expect((within(editor).getByRole("button", { name: "Close detail" }) as HTMLButtonElement).disabled).toBe(true);
  await user.click(within(editor).getByRole("button", { name: "Remove model" }));
  expect(runtime.setAgentRuntimeConfig).toHaveBeenCalledTimes(1);
  await act(async () => finish({ ...config, customModelProviders: [{ ...config.customModelProviders[0], models: [config.customModelProviders[0].models[1]] }], modelProviders: [{ ...config.modelProviders[0], models: [config.modelProviders[0].models[1]] }] }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(screen.queryByRole("button", { name: "model-one" })).toBeNull();
  expect(screen.getByRole("button", { name: "model-two" })).toBeDefined();
});

test.each(["credential", "model", "service"] as const)("a completed %s removal cannot update a settings page that has been left", async operation => {
  let finish!: (value: AgentRuntimeConfig) => void;
  runtime.getAgentRuntimeConfig.mockResolvedValue({ ...config, modelProviders: [{ ...config.modelProviders[0], credentialSource: "stored" }] });
  runtime.setAgentRuntimeConfig.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const onConfigured = vi.fn();
  const user = userEvent.setup();
  const view = render(<ModelsDialog onClose={() => {}} onConfigured={onConfigured} confirmAction={async () => true} />);
  await openProvider(user);
  onConfigured.mockClear();
  if (operation === "model") {
    await user.click(screen.getByRole("button", { name: "model-one" }));
    await user.click(within(await screen.findByRole("dialog", { name: "model-one" })).getByRole("button", { name: "Remove model" }));
  } else {
    await user.click(screen.getByRole("button", { name: "Service actions" }));
    await user.click(await screen.findByRole("menuitem", { name: operation === "credential" ? "Remove credential" : "Remove service" }));
  }
  await waitFor(() => expect(runtime.setAgentRuntimeConfig).toHaveBeenCalledTimes(1));
  view.unmount();
  await act(async () => finish(config));
  expect(onConfigured).not.toHaveBeenCalled();
});

test("model editing focuses its ID and Escape discards changes and returns to the model row", async () => {
  const user = renderModels();
  await openProvider(user);
  const row = screen.getByRole("button", { name: "model-one" });
  await user.click(row);
  const editor = await screen.findByRole("dialog", { name: "model-one" });
  const id = within(editor).getByRole("textbox", { name: /ID \*/ });
  await waitFor(() => expect(document.activeElement).toBe(id));
  await user.type(id, "-changed");
  await user.keyboard("{Escape}");
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  await waitFor(() => expect(document.activeElement).toBe(row));
  expect(runtime.setAgentRuntimeConfig).not.toHaveBeenCalled();
  expect(screen.queryByRole("button", { name: "model-one-changed" })).toBeNull();
});

test("an obsolete model test cannot clear a newer test's pending state or result", async () => {
  let finishOld!: (value: { httpStatus: number; latencyMs: number; outputPreview: string }) => void;
  let finishCurrent!: (value: { httpStatus: number; latencyMs: number; outputPreview: string }) => void;
  runtime.testAgentRuntimeModel
    .mockImplementationOnce(() => new Promise(resolve => { finishOld = resolve; }))
    .mockImplementationOnce(() => new Promise(resolve => { finishCurrent = resolve; }));
  const user = renderModels();
  await openProvider(user);
  await user.click(screen.getByRole("button", { name: "model-one" }));
  await user.click(within(await screen.findByRole("dialog", { name: "model-one" })).getByRole("button", { name: "Test" }));
  await user.keyboard("{Escape}");
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  await user.click(screen.getByRole("button", { name: "model-two" }));
  const current = await screen.findByRole("dialog", { name: "model-two" });
  await user.click(within(current).getByRole("button", { name: "Test" }));
  await act(async () => finishOld({ httpStatus: 200, latencyMs: 1, outputPreview: "obsolete result" }));
  expect(within(current).queryByText(/obsolete result/)).toBeNull();
  expect((within(current).getByRole("button", { name: "Testing…" }) as HTMLButtonElement).disabled).toBe(true);
  await act(async () => finishCurrent({ httpStatus: 200, latencyMs: 42, outputPreview: "current result" }));
  expect(await within(current).findByText("Connected · 42ms · HTTP 200 · current result")).toBeDefined();
  expect((within(current).getByRole("button", { name: "Test" }) as HTMLButtonElement).disabled).toBe(false);
});

test("cancelling an unsupported configuration reset keeps recovery actions available", async () => {
  runtime.getAgentRuntimeConfig.mockRejectedValueOnce(new Error("runtime_config_unsupported"));
  const confirmAction = vi.fn().mockResolvedValueOnce(false).mockResolvedValueOnce(true);
  runtime.resetAgentRuntimeConfig.mockResolvedValue({ config: { ...config, customModelProviders: [], modelProviders: [] } });
  const user = userEvent.setup();
  render(<ModelsDialog onClose={() => {}} confirmAction={confirmAction} />);
  await screen.findByText("runtime_config_unsupported");
  await user.click(screen.getByRole("button", { name: "Reset configuration…" }));
  expect(confirmAction).toHaveBeenCalledTimes(1);
  expect(runtime.resetAgentRuntimeConfig).not.toHaveBeenCalled();
  expect(screen.getByRole("button", { name: "Retry" })).toBeDefined();
  await user.click(screen.getByRole("button", { name: "Reset configuration…" }));
  expect(runtime.resetAgentRuntimeConfig).toHaveBeenCalledExactlyOnceWith();
  expect(await screen.findByText("Runtime configuration reset")).toBeDefined();
  expect(screen.getByRole("button", { name: "Choose a provider" })).toBeDefined();
});
