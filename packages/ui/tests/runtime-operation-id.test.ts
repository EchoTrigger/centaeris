import { beforeEach, expect, test, vi } from "vitest";

const host = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(async () => () => undefined),
}));

vi.mock("../src/host/hostBridge", () => ({
  invokeHost: host.invoke,
  isNativeHostRuntime: () => true,
  listenHost: host.listen,
}));

import {
  createRuntimeOperationId,
  createSession,
  sendAgentInput,
} from "../src/lib/chatBridge";

beforeEach(() => {
  host.invoke.mockReset();
  host.listen.mockClear();
  host.invoke.mockImplementation(async (command: string) => {
    if (command === "session/new") {
      return {
        id: "session-1",
        title: "Prompt",
        updatedAt: 1,
        sessionKind: "main",
        messageCount: 0,
      };
    }
    return {
      sessionId: "session-1",
      agentRunId: "agent-run-1",
      turnId: "turn-1",
      streamItems: [],
    };
  });
});

test("creates a fresh bounded opaque operation identity for each user action", () => {
  const first = createRuntimeOperationId();
  const second = createRuntimeOperationId();

  expect(first).toMatch(/^[0-9a-f-]{36}$/);
  expect(first.length).toBeLessThanOrEqual(128);
  expect(second).not.toBe(first);
});

test("forwards one session creation identity unchanged", async () => {
  await createSession("Prompt", "D:\\Workspace", "session-new:action-1");

  expect(host.invoke).toHaveBeenCalledWith("session/new", {
    request: {
      operationId: "session-new:action-1",
      title: "Prompt",
      cwd: "D:\\Workspace",
    },
  });
});

test("reuses the caller-owned prompt identity when the same action is retried", async () => {
  const request = {
    operationId: "session-prompt:action-1",
    sessionId: "session-1",
    message: "Retry me",
  };

  await sendAgentInput(request);
  await sendAgentInput(request);

  expect(host.invoke).toHaveBeenCalledTimes(2);
  expect(host.invoke.mock.calls.map(([, payload]) => payload)).toEqual([
    expect.objectContaining({
      request: expect.objectContaining({
        operationId: "session-prompt:action-1",
        sessionId: "session-1",
        message: "Retry me",
      }),
    }),
    expect.objectContaining({
      request: expect.objectContaining({
        operationId: "session-prompt:action-1",
        sessionId: "session-1",
        message: "Retry me",
      }),
    }),
  ]);
});
