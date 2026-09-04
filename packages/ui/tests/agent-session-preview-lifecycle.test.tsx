import type { ReactNode } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, test, vi } from "vitest";

const harness = vi.hoisted(() => ({
  buildSessionHydrationSnapshot: vi.fn(),
  openAgentStream: vi.fn(),
}));

vi.mock("../src/components/chat/chatRuntimeModel", () => ({
  applySessionEventToAssistantTurn: vi.fn(),
  buildSessionHydrationSnapshot: harness.buildSessionHydrationSnapshot,
  getSessionEventId: vi.fn(),
  getTerminalSessionEventStatus: vi.fn(),
  isRecord: (value: unknown) => typeof value === "object" && value !== null,
}));

vi.mock("../src/lib/chatBridge", () => ({
  openAgentStream: harness.openAgentStream,
}));

vi.mock("../src/components/chat/AgentResultStream", () => ({
  AgentResultStream: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
}));

import { AgentSessionPreview } from "../src/components/chat/AgentSessionPreview";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

test("retry replaces the failed preview load with one fresh lifecycle", async () => {
  harness.buildSessionHydrationSnapshot
    .mockRejectedValueOnce(new Error("first load failed"))
    .mockResolvedValueOnce({ messages: [], activeReplay: null });
  const rendered = { current: null as ReactTestRenderer | null };

  await act(async () => {
    rendered.current = create(<AgentSessionPreview sessionId="session-one" />);
    await Promise.resolve();
  });
  const renderer = rendered.current;
  if (!renderer) throw new Error("Agent preview did not render");
  expect(harness.buildSessionHydrationSnapshot).toHaveBeenCalledTimes(1);
  expect(renderer.root.findAllByType("button")).toHaveLength(1);

  await act(async () => {
    renderer!.root.findByType("button").props.onClick();
    await Promise.resolve();
  });

  expect(harness.buildSessionHydrationSnapshot).toHaveBeenCalledTimes(2);
  expect(harness.buildSessionHydrationSnapshot).toHaveBeenLastCalledWith("session-one");
  expect(renderer.root.findAllByType("button")).toHaveLength(0);

  await act(async () => renderer!.unmount());
});
