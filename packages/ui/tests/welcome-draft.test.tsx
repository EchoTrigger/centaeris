// @vitest-environment jsdom
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { cancelFrame, frame } from "motion/react";
import { ChatArea } from "../src/components/chat/ChatArea";
import { createSession, sendAgentInput } from "../src/lib/chatBridge";
vi.mock("../src/lib/chatBridge", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../src/lib/chatBridge")>()),
  getAgentRuntimeConfig: vi.fn(async () => ({
    executionHost: "localUser",
    modelProviderId: "test",
    model: "test",
    modelProviders: [],
    selectableModels: [
      {
        providerId: "test",
        providerName: "Test",
        model: "test",
        modelThinkingModes: [],
      },
    ],
    updatedAt: 1,
  })),
  createSession: vi.fn(),
  sendAgentInput: vi.fn(),
}));
vi.mock("../src/components/chat/useSessionHydration", () => ({
  useSessionHydration: () => ({ isHydratingSession: false, hydrationStage: "" }),
}));
beforeEach(() => {
  vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} }));
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
      unobserve() {}
    },
  );
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

test("a workspace-free draft cannot submit and survives choosing a workspace without remounting input", async () => {
  const user = userEvent.setup();
  const props = {
    currentSession: null,
    currentSessionId: null,
    workspaceName: "centaeris",
    onOpenWorkspace: vi.fn(async () => {}),
    onSelectWorkspace: vi.fn(async () => {}),
  };
  const view = render(
    <ChatArea {...props} workspaceRoot={null} workspaces={[]} />,
  );
  const input = screen.getByRole("textbox", { name: "Message" });
  await user.type(input, "Keep this draft{Enter}");
  expect(screen.getByRole("button", { name: "Send" })).toHaveProperty(
    "disabled",
    true,
  );
  expect(createSession).not.toHaveBeenCalled();
  expect(sendAgentInput).not.toHaveBeenCalled();
  view.rerender(
    <ChatArea {...props} workspaceRoot="D:/centaeris" workspaces={[]} />,
  );
  expect(screen.getByRole("textbox", { name: "Message" })).toBe(input);
  expect(input).toHaveProperty("value", "Keep this draft");
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Send" })).toHaveProperty(
      "disabled",
      false,
    ),
  );
  expect(screen.getByRole("button", { name: "Choose workspace: centaeris" }).textContent).toBe("centaeris");
  expect(screen.getByLabelText("Welcome")).toBeTruthy();
});

test("workspace selection, typing and clearing preserve the centered welcome", async () => {
  const user = userEvent.setup();
  const props = {
    currentSession: null,
    currentSessionId: null,
    workspaceName: "centaeris",
    workspaces: [],
    onOpenWorkspace: vi.fn(async () => {}),
    onSelectWorkspace: vi.fn(async () => {}),
  };
  const view = render(<ChatArea {...props} workspaceRoot={null} />);
  const input = screen.getByRole("textbox", { name: "Message" });
  expect(
    screen
      .getByRole("region", { name: "Message composer" })
      .getAttribute("data-position"),
  ).toBe("center");
  view.rerender(<ChatArea {...props} workspaceRoot="D:/centaeris" />);
  expect(
    screen
      .getByRole("region", { name: "Message composer" })
      .getAttribute("data-position"),
  ).toBe("center");
  expect(screen.getByRole("button", { name: "Choose workspace: centaeris" }).textContent).toBe("centaeris");
  expect(within(screen.getByLabelText("Welcome")).queryByText("centaeris")).toBeNull();
  expect(screen.getByRole("textbox", { name: "Message" })).toBe(input);
  await user.type(input, "Draft");
  expect(
    screen
      .getByRole("region", { name: "Message composer" })
      .getAttribute("data-position"),
  ).toBe("center");
  expect(screen.getByLabelText("Welcome")).toBeTruthy();
  await user.clear(input);
  expect(
    screen
      .getByRole("region", { name: "Message composer" })
      .getAttribute("data-position"),
  ).toBe("center");
  view.rerender(
    <ChatArea {...props} workspaceRoot="D:/centaeris" newChatRevision={1} />,
  );
  await waitFor(() =>
    expect(
      screen
        .getByRole("region", { name: "Message composer" })
        .getAttribute("data-position"),
    ).toBe("center"),
  );
  expect(screen.getByRole("textbox", { name: "Message" })).toHaveProperty(
    "value",
    "",
  );
});


test("selecting an existing conversation from New chat places the real composer at the bottom immediately", async () => {
  const props = { workspaceName: "centaeris", workspaceRoot: "D:/centaeris" };
  const view = render(<ChatArea {...props} currentSession={null} currentSessionId={null} />);
  const plane = screen.getByRole("region", { name: "Message composer" });
  const values: number[] = [];
  const sample = () => { values.push(Number.parseFloat(plane.style.top)); };
  frame.postRender(sample, true);
  try {
    view.rerender(<ChatArea {...props} currentSession={{ id: "existing", title: "Existing chat", messageCount: 1, cwd: "D:/centaeris", sessionKind: "main" }} currentSessionId="existing" />);
    await waitFor(() => expect(values.length).toBeGreaterThan(0));
    expect(values.every(value => value === 100)).toBe(true);
    expect(plane.getAttribute("data-position")).toBe("bottom");
  } finally { cancelFrame(sample); }
});
