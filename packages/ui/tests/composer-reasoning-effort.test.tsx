// @vitest-environment jsdom
import type { ComponentProps } from "react";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { ChatComposer } from "../src/components/chat/ChatComposer";
import type { SelectableModel } from "../src/lib/chatBridge";
vi.mock("../src/lib/chatBridge", () => ({ getNativeMcpCatalog: vi.fn(), configureNativeMcp: vi.fn() }));
afterEach(cleanup);
const first: SelectableModel = { providerId: "one", providerName: "Provider One", model: "shared", displayName: "Shared model", modelThinkingModes: ["low", "high", "max"] };
const second: SelectableModel = { ...first, providerId: "two", providerName: "Provider Two" };
const props = (): ComponentProps<typeof ChatComposer> => ({ panelResetKey: "session", inputValue: "", isStreaming: false, hasInput: false,
  modelRuntimeSummary: "Shared", selectableModels: [first, second], activeModelIndex: 0, runtimeConfigError: "", reasoningEffort: "high",
  reasoningEfforts: ["low", "high", "max"], contextUsage: null, compactInteractive: false, isCompacting: false,
  onInputChange: vi.fn(), onSubmit: vi.fn(), onComposerAction: vi.fn(), onModelSelect: vi.fn(), onReasoningEffortSelect: vi.fn(), onCompact: vi.fn() });

test("portaled model choices preserve provider identity and close after selecting", async () => {
  const user = userEvent.setup(), values = props();
  render(<ChatComposer {...values} />);
  await user.click(screen.getByRole("combobox", { name: "Global models" }));
  const options = await screen.findAllByRole("option", { name: "Shared model" });
  expect(options).toHaveLength(2);
  await user.click(options[1]);
  expect(values.onModelSelect).toHaveBeenCalledExactlyOnceWith(second);
  await waitFor(() => expect(screen.queryByRole("listbox")).toBeNull());
});

test("reasoning uses concrete efforts and keyboard selection without inventing a provider default", async () => {
  const user = userEvent.setup(), values = props();
  const view = render(<ChatComposer {...values} reasoningEffort={null} reasoningEfforts={[]} />);
  expect(screen.queryByRole("combobox", { name: "Reasoning effort" })).toBeNull();
  expect(document.body.textContent).not.toContain("provider default");
  view.rerender(<ChatComposer {...values} />);
  await user.click(screen.getByRole("combobox", { name: "Reasoning effort" }));
  const options = await screen.findAllByRole("option");
  expect(options.map((option) => option.getAttribute("aria-selected"))).toEqual(["false", "true", "false"]);
  // Floating focus is scheduled after the portal mounts. Wait for the
  // selected option to receive it before exercising keyboard navigation.
  await waitFor(() => expect(document.activeElement).toBe(options[1]));
  await user.keyboard("{End}");
  await waitFor(() => expect(document.activeElement).toBe(options[2]));
  await user.keyboard("{Enter}");
  await waitFor(() => expect(values.onReasoningEffortSelect).toHaveBeenCalledExactlyOnceWith("max"));
  expect(screen.queryByRole("listbox")).toBeNull();
});

test("context details keep finite percentages and stay open when their portaled contents are clicked", async () => {
  const user = userEvent.setup(), values = props();
  values.contextUsage = { sessionId: "session", usedTokens: 0, maxContextTokens: 0, usedPercentage: 0, isCompacting: false,
    breakdown: { systemPromptTokens: 0, systemToolTokens: 0, mcpToolTokens: 12, skillsTokens: 0, messageTokens: 0, autoCompactBufferTokens: 0, freeSpaceTokens: 0,
      mcpTools: [{ providerId: "one", name: "search", tokens: 12 }] } };
  render(<ChatComposer {...values} />);
  await user.click(screen.getByRole("button", { name: "Context window" }));
  await user.click(await screen.findByText("MCP tools", { selector: "summary" }));
  expect(document.body.textContent).toContain("one · search");
  expect(document.body.textContent).not.toContain("NaN");
  expect(document.body.textContent).not.toContain("Infinity");
  expect(screen.getByRole("button", { name: "Context window" }).getAttribute("aria-expanded")).toBe("true");
  await user.keyboard("{Escape}");
  await waitFor(() => expect(screen.getByRole("button", { name: "Context window" }).getAttribute("aria-expanded")).toBe("false"));
});
