// @vitest-environment jsdom
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { AgentRuntimeConfig } from "../src/lib/chatBridge";

const runtime = vi.hoisted(() => ({
  getAgentRuntimeConfig: vi.fn(),
  testAgentRuntimeModel: vi.fn(),
  setAgentRuntimeConfig: vi.fn(),
}));
vi.mock("../src/lib/chatBridge", () => ({
  getAgentRuntimeConfig: runtime.getAgentRuntimeConfig,
  resetAgentRuntimeConfig: vi.fn(),
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
