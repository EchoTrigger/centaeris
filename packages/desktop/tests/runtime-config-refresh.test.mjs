import { expect, test, vi } from "vitest";
import { isNativeHostRuntime, listenHost } from "../src/renderer/host/hostBridge";
import { listenAgentRuntimeConfigChanges } from "../src/renderer/lib/chatBridge";

vi.mock("../src/renderer/host/hostBridge", () => ({
  invokeHost: vi.fn(),
  isNativeHostRuntime: vi.fn(() => true),
  listenHost: vi.fn(),
}));

test("runtime config notifications reject invalid payloads and retain unsubscribe ownership", async () => {
  const handler = vi.fn();
  const unsubscribe = vi.fn();
  vi.mocked(listenHost).mockResolvedValue(unsubscribe);
  expect(await listenAgentRuntimeConfigChanges(handler)).toBe(unsubscribe);
  const [event, notify] = vi.mocked(listenHost).mock.calls[0];
  expect(event).toBe("runtime/config-changed");
  notify({});
  expect(handler).toHaveBeenCalledTimes(1);
  for (const payload of [null, [], "changed", { revision: 1 }]) {
    expect(() => notify(payload)).toThrow();
  }
  expect(handler).toHaveBeenCalledTimes(1);
  vi.mocked(listenHost).mockClear();
  vi.mocked(isNativeHostRuntime).mockReturnValue(false);
  expect(await listenAgentRuntimeConfigChanges(handler)).toBeTypeOf("function");
  expect(listenHost).not.toHaveBeenCalled();
});
