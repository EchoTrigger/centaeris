import { beforeEach, expect, test, vi } from "vitest";
const host = vi.hoisted(() => ({ invokeHost: vi.fn(), isNativeHostRuntime: vi.fn(() => true) }));
vi.mock("../src/host/hostBridge", () => ({ ...host, listenHost: vi.fn() }));
import { listPlugins, getPluginDetail, setPluginEnabled, getPluginSourceRef, reloadPlugins } from "../src/lib/chatBridge";
beforeEach(() => { host.invokeHost.mockReset(); host.isNativeHostRuntime.mockReturnValue(true); });
test.each([
  ["plugin/list", () => listPlugins(), {}],
  ["plugin/detail", () => getPluginDetail({ id: "example" }), { id: "example" }],
  ["plugin/set_enabled", () => setPluginEnabled({ id: "example", enabled: false }), { id: "example", enabled: false }],
  ["plugin/source_ref", () => getPluginSourceRef({ id: "example" }), { id: "example" }],
  ["plugin/reload", () => reloadPlugins(), {}],
])("%s forwards its exact request and host response", async (command, call, request) => {
  const response = { marker: "host response" };
  host.invokeHost.mockResolvedValue(response);
  expect(await call()).toBe(response);
  expect(host.invokeHost).toHaveBeenCalledExactlyOnceWith(command, { request });
});
test("plugin access outside the native host fails before invocation", async () => {
  host.isNativeHostRuntime.mockReturnValue(false);
  for (const call of [listPlugins, () => getPluginDetail({id:"p"}), () => setPluginEnabled({id:"p",enabled:true}), () => getPluginSourceRef({id:"p"}), reloadPlugins]) await expect(call()).rejects.toThrow("desktop-only");
  expect(host.invokeHost).not.toHaveBeenCalled();
});
