// @vitest-environment jsdom
import { useState, type ComponentProps } from "react";
import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { readFileSync } from "node:fs";
import { ChatComposer } from "../src/components/chat/ChatComposer";
import type { SelectableModel } from "../src/lib/chatBridge";
vi.mock("../src/lib/chatBridge", () => ({
  getNativeMcpCatalog: vi.fn(),
  configureNativeMcp: vi.fn(),
}));
afterEach(cleanup);
const first: SelectableModel = {
  providerId: "one",
  providerName: "Provider One",
  model: "shared",
  displayName: "Shared model",
  modelThinkingModes: ["low", "high", "max"],
};
const second: SelectableModel = {
  ...first,
  providerId: "two",
  providerName: "Provider Two",
};
const props = (): ComponentProps<typeof ChatComposer> => ({
  panelResetKey: "session",
  inputValue: "",
  isStreaming: false,
  hasInput: false,
  modelRuntimeSummary: "Shared",
  selectableModels: [first, second],
  activeModelIndex: 0,
  runtimeConfigError: "",
  reasoningEffort: "high",
  reasoningEfforts: ["low", "high", "max"],
  contextUsage: null,
  compactInteractive: false,
  isCompacting: false,
  onInputChange: vi.fn(),
  onSubmit: vi.fn(),
  onComposerAction: vi.fn(),
  onModelSelect: vi.fn(),
  onReasoningEffortSelect: vi.fn(),
  onCompact: vi.fn(),
});

test("portaled model choices preserve provider identity and close after selecting", async () => {
  const user = userEvent.setup(),
    values = props();
  render(<ChatComposer {...values} />);
  expect(screen.getByRole("textbox", { name: "Message" })).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Global models" }));
  const options = await screen.findAllByRole("menuitemradio", {
    name: "Shared model",
  });
  expect(options).toHaveLength(2);
  await user.click(options[1]);
  expect(values.onModelSelect).toHaveBeenCalledExactlyOnceWith(second);
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
});

test("models without thinking modes omit the Effort submenu", async () => {
  const user = userEvent.setup();
  render(<ChatComposer {...props()} reasoningEffort={null} reasoningEfforts={[]} />);
  await user.click(screen.getByRole("button", { name: "Global models" }));
  await screen.findByRole("menu");
  expect(screen.queryByRole("menuitem", { name: "Reasoning effort" })).toBeNull();
});

test("Effort is a separate submenu with concrete choices and keyboard selection", async () => {
  const user = userEvent.setup(),
    values = props();
  render(<ChatComposer {...values} />);
  await user.click(screen.getByRole("button", { name: "Global models" }));
  await user.click(
    await screen.findByRole("menuitem", { name: "Reasoning effort" }),
  );
  const submenu = await screen.findByRole("menu", { name: "Reasoning effort" });
  const options = within(submenu).getAllByRole("menuitemradio");
  expect(options.map((option) => option.getAttribute("aria-checked"))).toEqual([
    "false",
    "true",
    "false",
  ]);
  await user.keyboard("{ArrowRight}");
  await waitFor(() =>
    expect(submenu.contains(document.activeElement)).toBe(true),
  );
  await user.keyboard("{End}{Enter}");
  await waitFor(() =>
    expect(values.onReasoningEffortSelect).toHaveBeenCalledExactlyOnceWith(
      "max",
    ),
  );
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
});

test("context details keep finite token counts and stay open when their portaled contents are clicked", async () => {
  const user = userEvent.setup(),
    values = props();
  values.contextUsage = {
    sessionId: "session",
    usedTokens: 0,
    maxContextTokens: 0,
    usedPercentage: 0,
    isCompacting: false,
    breakdown: {
      systemPromptTokens: 0,
      systemToolTokens: 0,
      mcpToolTokens: 12,
      skillsTokens: 0,
      messageTokens: 0,
      autoCompactBufferTokens: 0,
      freeSpaceTokens: 0,
      mcpTools: [{ providerId: "one", name: "search", tokens: 12 }],
    },
  };
  render(<ChatComposer {...values} />);
  await user.click(screen.getByRole("button", { name: "Context window" }));
  await user.click(
    await screen.findByText("MCP tools", { selector: "summary" }),
  );
  expect(document.body.textContent).toContain("one · search");
  expect(document.body.textContent).not.toContain("NaN");
  expect(document.body.textContent).not.toContain("Infinity");
  expect(
    screen
      .getByRole("button", { name: "Context window" })
      .getAttribute("aria-expanded"),
  ).toBe("true");
  await user.keyboard("{Escape}");
  await waitFor(() =>
    expect(
      screen
        .getByRole("button", { name: "Context window" })
        .getAttribute("aria-expanded"),
    ).toBe("false"),
  );
});

test("slash resource configuration uses the in-place handler without a permanent menu", async () => {
  const user = userEvent.setup(),
    values = props();
  values.inputValue = "/models";
  values.hasInput = true;
  values.onOpenResource = vi.fn();
  render(<ChatComposer {...values} />);
  expect(
    screen.queryByRole("button", { name: "Composer commands" }),
  ).toBeNull();
  await user.click(
    await screen.findByRole("option", {
      name: /models.*Manage model providers/,
    }),
  );
  expect(values.onOpenResource).toHaveBeenCalledExactlyOnceWith("models");
  expect(values.onInputChange).toHaveBeenCalledExactlyOnceWith("");
});

test("context popup stays in its library positioner's measurement flow with the real chat styles", async () => {
  const sheet = document.createElement("style");
  // App imports the shell first; main then imports the index layers.
  // Inline those local layout sheets in runtime order for computed styles.
  sheet.textContent = [
    "../src/styles/desktop-chat.css",
    "../src/styles/desktop-shell.css",
    "../src/styles/frontend-foundations.css",
    "../src/styles/chat.css",
    "../src/index.css",
  ]
    .map((path) =>
      readFileSync(new URL(path, import.meta.url), "utf8").replace(
        /^@import[^;]+;/gm,
        "",
      ),
    )
    .join("\n");
  document.head.append(sheet);
  try {
    expect(sheet.sheet?.cssRules.length).toBeGreaterThan(0);
    const user = userEvent.setup();
    render(<ChatComposer {...props()} />);
    await user.click(screen.getByRole("button", { name: "Context window" }));
    const popup = await screen.findByRole("dialog", { name: "Context window" });
    const computed = getComputedStyle(popup);
    // Floating UI owns anchor placement on the positioner. The popup must
    // contribute its size to that parent instead of applying a second offset.
    expect(["", "static", "relative"]).toContain(computed.position);
    // jsdom does not expand inset into its longhands during the cascade.
    // The explicit shorthand is the browser contract clearing legacy offsets.
    if (computed.position === "relative") expect(computed.inset).toBe("auto");
  } finally {
    sheet.remove();
  }
});

test("slash effort opens the model menu and its Effort submenu in place", async () => {
  const user = userEvent.setup(),
    values = props();
  function Harness() {
    const [input, setInput] = useState("/effort");
    return (
      <ChatComposer
        {...values}
        inputValue={input}
        hasInput={Boolean(input)}
        onInputChange={setInput}
      />
    );
  }
  render(<Harness />);
  await user.click(
    await screen.findByRole("option", { name: /effort.*Set reasoning effort/ }),
  );
  expect(
    await screen.findByRole("menu", { name: "Reasoning effort" }),
  ).toBeTruthy();
  await user.click(screen.getByRole("menuitemradio", { name: "Low" }));
  expect(values.onReasoningEffortSelect).toHaveBeenCalledExactlyOnceWith("low");
  expect(values.onSubmit).not.toHaveBeenCalled();
});
