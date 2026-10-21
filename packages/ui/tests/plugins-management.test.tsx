// @vitest-environment jsdom
import { act } from "react";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import * as bridge from "../src/lib/chatBridge";
import { PluginsDialog } from "../src/components/PluginsDialog";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "../src/components/ui/tabs";
vi.mock("../src/lib/chatBridge", () => ({ listPlugins: vi.fn(), getPluginDetail: vi.fn(), removePlugin: vi.fn(), reloadPlugins: vi.fn(), revealPluginSourceRef: vi.fn(), selectAndInstallPlugin: vi.fn(), setPluginEnabled: vi.fn() }));
const plugin = (id: string, enabled = true, source = "managed"): bridge.PluginDescriptorV1 => ({ id, name: `${id} plugin`, description: `Tools for ${id}`, source, enabled, path: `/plugins/${id}`, errors: [], tools: ["inspect"], scopes: ["user"], activationStatus: "active", policySource: "user" });
const detail = (item: bridge.PluginDescriptorV1): bridge.PluginDetailV1 => ({ descriptor: item, capabilities: { skills: ["Review code"], cli: [], mcpServers: ["repo-tools"], apps: [], hooks: [], capabilities: ["inspect"] } });
beforeEach(() => { Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true }); });
beforeEach(() => {
  vi.mocked(bridge.listPlugins).mockResolvedValue([plugin("alpha"), plugin("beta", false, "workspace")]);
  vi.mocked(bridge.getPluginDetail).mockImplementation(async ({ id }) => detail(plugin(id)));
  vi.mocked(bridge.reloadPlugins).mockResolvedValue({ schema: "plugin.catalog.v1", enabledPlugins: ["alpha"], disabledPlugins: ["beta"] });
  vi.mocked(bridge.setPluginEnabled).mockResolvedValue({ schema: "plugin.catalog.v1", enabledPlugins: [], disabledPlugins: ["alpha"] });
  vi.mocked(bridge.selectAndInstallPlugin).mockResolvedValue({ cancelled: true, plugin: null });
  vi.mocked(bridge.revealPluginSourceRef).mockResolvedValue({ kind: "local_path", path: "/plugins/alpha", opened: true });
});
afterEach(() => { cleanup(); vi.resetAllMocks(); });

test("plugin search, status filtering and detail navigation retain the list context", async () => {
  const user = userEvent.setup(); render(<PluginsDialog />);
  await screen.findByRole("button", { name: "Open alpha plugin" });
  await user.type(screen.getByRole("textbox", { name: "Search plugins" }), "alpha");
  expect(screen.queryByRole("button", { name: "Open beta plugin" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Open alpha plugin" }));
  await screen.findByRole("heading", { name: /^MCP servers/ });
  expect(document.activeElement).toBe(screen.getByRole("heading", { name: "alpha plugin" }));
  expect(screen.getByText("/plugins/alpha")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Back to plugins" }));
  expect(document.activeElement).toBe(screen.getByRole("heading", { name: "Plugins" }));
  expect(screen.getByRole("textbox", { name: "Search plugins" })).toHaveProperty("value", "alpha");
  await user.clear(screen.getByRole("textbox", { name: "Search plugins" }));
  await user.click(screen.getByRole("combobox", { name: "Plugin status" }));
  await user.click(await screen.findByRole("option", { name: "Disabled" }));
  expect(screen.queryByRole("button", { name: "Open alpha plugin" })).toBeNull();
  expect(screen.getByRole("button", { name: "Open beta plugin" })).toBeTruthy();
});

test("a standard switch sends one explicit policy change and removal requires confirmation", async () => {
  const user = userEvent.setup(); render(<PluginsDialog />);
  await user.click(await screen.findByRole("button", { name: "Open alpha plugin" }));
  await user.click(screen.getByRole("switch", { name: "Disable plugin" }));
  await waitFor(() => expect(bridge.setPluginEnabled).toHaveBeenCalledExactlyOnceWith({ id: "alpha", enabled: false }));
  await waitFor(() => expect(screen.getByRole("button", { name: "Remove plugin" })).toHaveProperty("disabled", false));
  await user.click(screen.getByRole("button", { name: "Remove plugin" }));
  await screen.findByRole("alertdialog", { name: "Remove this plugin?" });
  await user.click(screen.getByRole("button", { name: "Cancel" }));
  expect(bridge.removePlugin).not.toHaveBeenCalled();
  vi.mocked(bridge.removePlugin).mockResolvedValue({ removedId: "alpha", catalog: { schema: "plugin.catalog.v1", enabledPlugins: [], disabledPlugins: [] } });
  vi.mocked(bridge.listPlugins).mockResolvedValue([plugin("beta", false, "workspace")]);
  await user.click(screen.getByRole("button", { name: "Remove plugin" }));
  await user.click(await screen.findByRole("button", { name: "Confirm" }));
  await waitFor(() => expect(bridge.removePlugin).toHaveBeenCalledExactlyOnceWith({ id: "alpha" }));
  await screen.findByRole("heading", { name: "Plugins" });
  expect(screen.queryByRole("button", { name: "Open alpha plugin" })).toBeNull();
});

test("installation cancellation preserves the catalog and a confirmed receipt survives refresh failure", async () => {
  const user = userEvent.setup(); render(<PluginsDialog />);
  await screen.findByRole("button", { name: "Open alpha plugin" });
  await user.click(screen.getByRole("button", { name: "Install plugin" }));
  await screen.findByText("Installation cancelled.");
  expect(screen.getByRole("button", { name: "Open alpha plugin" })).toBeTruthy();
  vi.mocked(bridge.selectAndInstallPlugin).mockResolvedValue({ cancelled: false, plugin: detail(plugin("new")) });
  vi.mocked(bridge.listPlugins).mockRejectedValue(new Error("catalog disconnected"));
  await user.click(screen.getByRole("button", { name: "Install plugin" }));
  await screen.findByRole("heading", { name: "new plugin" });
  expect(screen.getByText("new plugin installed.")).toBeTruthy();
  expect(await screen.findByRole("alert")).toHaveProperty("textContent", expect.stringContaining("catalog disconnected"));
  expect(bridge.selectAndInstallPlugin).toHaveBeenCalledTimes(2);
});

test("owner-managed packages cannot be removed and detail failures can be retried", async () => {
  const user = userEvent.setup(); render(<PluginsDialog />);
  vi.mocked(bridge.getPluginDetail).mockRejectedValueOnce(new Error("manifest unavailable"));
  await user.click(await screen.findByRole("button", { name: "Open beta plugin" }));
  expect(screen.queryByRole("button", { name: "Remove plugin" })).toBeNull();
  await screen.findByRole("alert");
  await user.click(screen.getByRole("button", { name: "Retry" }));
  await screen.findByRole("heading", { name: /^MCP servers/ });
});

test("returning to the catalog invalidates an old detail request", async () => {
  let finish!: (detail: bridge.PluginDetailV1) => void;
  vi.mocked(bridge.getPluginDetail).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const user = userEvent.setup(); render(<PluginsDialog />);
  await user.click(await screen.findByRole("button", { name: "Open alpha plugin" }));
  await user.click(screen.getByRole("button", { name: "Back to plugins" }));
  await act(async () => finish(detail(plugin("alpha"))));
  expect(screen.getByRole("heading", { name: "Plugins" })).toBeTruthy();
  expect(screen.queryByRole("heading", { name: /^MCP servers/ })).toBeNull();
});


test("a direct plugin link can retry its detail failure before the catalog knows the plugin", async () => {
  vi.mocked(bridge.listPlugins).mockResolvedValue([]);
  vi.mocked(bridge.getPluginDetail).mockRejectedValueOnce(new Error("Detail unavailable"));
  const user = userEvent.setup(); render(<PluginsDialog selectedPluginId="alpha" />);
  await screen.findByText(/Unable to load plugin details/);
  await user.click(screen.getByRole("button", { name: "Retry" }));
  await screen.findByRole("heading", { name: "alpha plugin" });
});


test("activating a resource tab keeps keyboard focus in its tab list", async () => {
  const user = userEvent.setup();
  render(<Tabs defaultValue="other"><TabsList><TabsTrigger value="other">Other</TabsTrigger><TabsTrigger value="plugins">Plugins tab</TabsTrigger></TabsList><TabsContent value="other">Other content</TabsContent><TabsContent value="plugins"><PluginsDialog /></TabsContent></Tabs>);
  await user.click(screen.getByRole("tab", { name: "Plugins tab" }));
  await screen.findByRole("button", { name: "Open alpha plugin" });
  expect(document.activeElement).toBe(screen.getByRole("tab", { name: "Plugins tab" }));
});

test.each(["detail", "catalog-again"] as const)("installation receipts preserve subsequent %s navigation", async destination => {
  let finish!: (receipt: { cancelled: boolean; plugin: bridge.PluginDetailV1 }) => void;
  vi.mocked(bridge.selectAndInstallPlugin).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const user = userEvent.setup(); render(<PluginsDialog />);
  await screen.findByRole("button", { name: "Open alpha plugin" });
  await user.click(screen.getByRole("button", { name: "Install plugin" }));
  await user.click(screen.getByRole("button", { name: "Open beta plugin" }));
  await screen.findByRole("heading", { name: "beta plugin" });
  if (destination === "catalog-again") await user.click(screen.getByRole("button", { name: "Back to plugins" }));
  await act(async () => finish({ cancelled: false, plugin: detail(plugin("new")) }));
  expect(screen.getByRole("heading", { name: destination === "detail" ? "beta plugin" : "Plugins" })).toBeTruthy();
  expect(screen.getByText("new plugin installed.")).toBeTruthy();
  if (destination === "detail") expect(screen.getByRole("heading", { name: /^MCP servers/ })).toBeTruthy();
});

test.each(["different-detail", "same-detail-again", "current-detail"] as const)("mutation errors respect the %s visit and leave retry available", async destination => {
  let reject!: (failure: Error) => void;
  vi.mocked(bridge.setPluginEnabled).mockImplementationOnce(() => new Promise((_resolve, fail) => { reject = fail; }));
  const user = userEvent.setup(); render(<PluginsDialog />);
  await user.click(await screen.findByRole("button", { name: "Open alpha plugin" }));
  await user.click(screen.getByRole("switch", { name: "Disable plugin" }));
  if (destination !== "current-detail") {
    await user.click(screen.getByRole("button", { name: "Back to plugins" }));
    await user.click(screen.getByRole("button", { name: destination === "different-detail" ? "Open beta plugin" : "Open alpha plugin" }));
    await screen.findByRole("heading", { name: destination === "different-detail" ? "beta plugin" : "alpha plugin" });
  }
  await act(async () => reject(new Error("policy write unavailable")));
  if (destination === "current-detail") expect(screen.getByText("policy write unavailable")).toBeTruthy();
  else expect(screen.queryByText("policy write unavailable")).toBeNull();
  await user.click(screen.getByRole("switch"));
  expect(bridge.setPluginEnabled).toHaveBeenCalledTimes(2);
  expect(vi.mocked(bridge.setPluginEnabled).mock.calls[1][0]).toEqual({ id: destination === "different-detail" ? "beta" : "alpha", enabled: destination === "different-detail" });
});

test("controlled history navigation invalidates mutation failures even after returning to the same plugin", async () => {
  let reject!: (failure: Error) => void;
  vi.mocked(bridge.setPluginEnabled).mockImplementationOnce(() => new Promise((_resolve, fail) => { reject = fail; }));
  const user = userEvent.setup();
  const { rerender } = render(<PluginsDialog selectedPluginId="alpha" />);
  await screen.findByRole("heading", { name: "alpha plugin" });
  await user.click(screen.getByRole("switch", { name: "Disable plugin" }));
  rerender(<PluginsDialog selectedPluginId="beta" />);
  await screen.findByRole("heading", { name: "beta plugin" });
  rerender(<PluginsDialog selectedPluginId="alpha" />);
  await screen.findByRole("heading", { name: "alpha plugin" });
  await act(async () => reject(new Error("obsolete controlled navigation error")));
  expect(screen.queryByText("obsolete controlled navigation error")).toBeNull();
  await user.click(screen.getByRole("switch"));
  expect(bridge.setPluginEnabled).toHaveBeenCalledTimes(2);
});
