// @vitest-environment jsdom
import { act, useState, type ComponentProps } from "react";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { ChatComposer } from "../src/components/chat/ChatComposer";
import { configureNativeMcp, getNativeMcpCatalog, type NativeMcpCatalog, type NativeMcpServer } from "../src/lib/chatBridge";
vi.mock("../src/lib/chatBridge", () => ({ configureNativeMcp: vi.fn(), getNativeMcpCatalog: vi.fn() }));
function deferred<T>() { let resolve!: (value: T) => void, reject!: (error: Error) => void; const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }
const server = (id: string, configured = false): NativeMcpServer => ({ pluginName: "plugin", pluginDisplayName: "Tools", serverId: id, pluginEnabled: true, status: configured ? "ready" : "needsConfiguration", configurable: true, configured, transport: "streamableHttp", endpoint: "https://example.test/mcp", toolNames: ["search"] });
const catalog = (configured?: string): NativeMcpCatalog => ({ schema: "native.mcp.catalog.v1", servers: [server("first", configured === "first"), server("second", configured === "second")] });
const base = (): Omit<ComponentProps<typeof ChatComposer>, "inputValue" | "hasInput" | "onInputChange" | "panelResetKey"> => ({ isStreaming: false, modelRuntimeSummary: "Model", selectableModels: [{ providerId: "provider", providerName: "Provider", model: "model", displayName: "Model", modelThinkingModes: [] }], activeModelIndex: 0, runtimeConfigError: "", reasoningEffort: null, reasoningEfforts: [], contextUsage: null, compactInteractive: false, isCompacting: false, onSubmit: vi.fn(), onComposerAction: vi.fn(), onModelSelect: vi.fn(), onReasoningEffortSelect: vi.fn(), onCompact: vi.fn() });
function Harness({ session = "one" }: { session?: string }) { const [input, setInput] = useState(""); return <><button type="button">Outside</button><ChatComposer {...base()} panelResetKey={session} inputValue={input} hasInput={Boolean(input)} onInputChange={setInput} /></>; }
const prompt = () => screen.getByRole("textbox");
async function openCatalog(user: ReturnType<typeof userEvent.setup>) { await user.type(prompt(), "/mcp"); await user.keyboard("{Enter}"); await screen.findByRole("option", { name: /first/ }); await waitFor(() => expect(document.activeElement).toBe(prompt())); }
async function openServer(user: ReturnType<typeof userEvent.setup>, id: string) { await user.click(screen.getByRole("option", { name: new RegExp(id) })); await screen.findByLabelText("API key"); }
async function save(user: ReturnType<typeof userEvent.setup>, value = "local-credential") { await user.type(screen.getByLabelText("API key"), value); await user.click(screen.getByRole("button", { name: "Save & test" })); }
beforeEach(() => { Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true }); vi.mocked(getNativeMcpCatalog).mockResolvedValue(catalog()); });
afterEach(() => { cleanup(); vi.resetAllMocks(); });

test.each(["outside", "model"])("a completed MCP save cannot reopen or steal focus after leaving via %s", async departure => {
  const request = deferred<NativeMcpCatalog>(); vi.mocked(configureNativeMcp).mockReturnValue(request.promise);
  const user = userEvent.setup(); render(<Harness />); await openCatalog(user); await openServer(user, "first"); await save(user);
  if (departure === "model") { await user.click(screen.getByRole("button", { name: "Global models" })); await user.keyboard("{ArrowDown}"); await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("menuitemradio", { name: "Model" }))); }
  else await user.click(screen.getByRole("button", { name: "Outside" }));
  const focus = document.activeElement;
  await act(async () => request.resolve(catalog("first")));
  expect(screen.queryByRole("region", { name: "MCP servers" })).toBeNull();
  expect(screen.queryByRole("region", { name: "Configure MCP server" })).toBeNull();
  expect(document.activeElement).toBe(focus);
  if (departure === "model") expect(screen.getByRole("menuitemradio", { name: "Model" })).toBeTruthy();
  expect(configureNativeMcp).toHaveBeenCalledOnce();
});

test.each(["success", "failure"])("a departed server's %s does not change another server's draft, error or busy state", async outcome => {
  const first = deferred<NativeMcpCatalog>(), second = deferred<NativeMcpCatalog>(); vi.mocked(configureNativeMcp).mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
  const user = userEvent.setup(); render(<Harness />); await openCatalog(user); await openServer(user, "first"); await save(user);
  await user.click(screen.getByRole("button", { name: "Back to MCP servers" })); await openServer(user, "second");
  expect(screen.getByLabelText("API key")).toHaveProperty("disabled", false);
  await save(user, "second-credential");
  const focus = document.activeElement;
  await act(async () => { if (outcome === "success") first.resolve(catalog("first")); else first.reject(new Error("Old server failed")); });
  expect(screen.getByLabelText("API key")).toHaveProperty("value", "second-credential");
  expect(screen.getByRole("button", { name: "Testing…" })).toHaveProperty("disabled", true);
  expect(screen.queryByText("Old server failed")).toBeNull(); expect(document.activeElement).toBe(focus);
  await act(async () => second.resolve(catalog("second")));
  await screen.findByRole("option", { name: /second.*Configured/ });
  await waitFor(() => expect(document.activeElement).toBe(prompt()));
});

test("Escape permits leaving a save while a later failure remains owned by its original configuration", async () => {
  const request = deferred<NativeMcpCatalog>(); vi.mocked(configureNativeMcp).mockReturnValue(request.promise);
  const user = userEvent.setup(); render(<Harness />); await openCatalog(user); await openServer(user, "first"); await save(user);
  await user.keyboard("{Escape}"); await openServer(user, "second"); await user.type(screen.getByLabelText("API key"), "second-draft");
  await act(async () => request.reject(new Error("Original request failed")));
  expect(screen.getByLabelText("API key")).toHaveProperty("value", "second-draft");
  expect(screen.getByRole("button", { name: "Save & test" })).toHaveProperty("disabled", false);
  expect(screen.queryByText("Original request failed")).toBeNull();
});

test("session reset isolates a new configuration from the old save receipt", async () => {
  const old = deferred<NativeMcpCatalog>(), current = deferred<NativeMcpCatalog>(); vi.mocked(configureNativeMcp).mockReturnValueOnce(old.promise).mockReturnValueOnce(current.promise);
  const user = userEvent.setup(), view = render(<Harness />); await openCatalog(user); await openServer(user, "first"); await save(user);
  view.rerender(<Harness session="two" />); await openCatalog(user); await openServer(user, "second"); await save(user, "new-session-credential");
  await act(async () => old.resolve(catalog("first")));
  expect(screen.getByLabelText("API key")).toHaveProperty("value", "new-session-credential");
  expect(screen.getByRole("button", { name: "Testing…" })).toHaveProperty("disabled", true);
  await act(async () => current.resolve(catalog("second")));
  await screen.findByRole("option", { name: /second.*Configured/ });
});

test("reopening the same server never duplicates its unresolved save", async () => {
  const request = deferred<NativeMcpCatalog>(); vi.mocked(configureNativeMcp).mockReturnValue(request.promise);
  const user = userEvent.setup(); render(<Harness />); await openCatalog(user); await openServer(user, "first"); await save(user);
  await user.click(screen.getByRole("button", { name: "Cancel" })); await openServer(user, "first");
  await user.click(screen.getByRole("button", { name: "Testing…" })); expect(configureNativeMcp).toHaveBeenCalledOnce();
  await act(async () => request.resolve(catalog("first")));
  expect(screen.getByLabelText("API key")).toHaveProperty("disabled", false);
  expect(screen.getByLabelText("API key")).toHaveProperty("value", "");
});

test("an uncertain current save retains its draft and retries only on a fresh explicit submit", async () => {
  vi.mocked(configureNativeMcp).mockRejectedValueOnce(new Error("Outcome unknown; check configuration before retrying")).mockResolvedValueOnce(catalog("first"));
  const user = userEvent.setup(); render(<Harness />); await openCatalog(user); await openServer(user, "first"); await save(user);
  await screen.findByText("Outcome unknown; check configuration before retrying");
  expect(screen.getByLabelText("API key")).toHaveProperty("value", "local-credential"); expect(configureNativeMcp).toHaveBeenCalledOnce();
  await user.click(screen.getByRole("button", { name: "Save & test" })); await screen.findByRole("option", { name: /first.*Configured/ });
  expect(configureNativeMcp).toHaveBeenCalledTimes(2);
});


test("a known save receipt refreshes a catalog reopened while the write was pending", async () => {
  const request = deferred<NativeMcpCatalog>(); vi.mocked(configureNativeMcp).mockReturnValue(request.promise);
  const user = userEvent.setup(); render(<Harness />); await openCatalog(user); await openServer(user, "first"); await save(user);
  await user.click(screen.getByRole("button", { name: "Outside" })); await openCatalog(user);
  expect(screen.getByRole("region", { name: "MCP servers" })).toBeTruthy();
  await act(async () => request.resolve(catalog("first")));
  expect(screen.getByRole("option", { name: /first.*Configured/ })).toBeTruthy();
});


test("an older save receipt cannot replace a newer acknowledged catalog", async () => {
  const first = deferred<NativeMcpCatalog>(), second = deferred<NativeMcpCatalog>(); vi.mocked(configureNativeMcp).mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
  const user = userEvent.setup(); render(<Harness />); await openCatalog(user); await openServer(user, "first"); await save(user);
  await user.click(screen.getByRole("button", { name: "Back to MCP servers" })); await openServer(user, "second"); await save(user, "second-credential");
  await act(async () => second.resolve(catalog("second"))); await screen.findByRole("option", { name: /second.*Configured/ });
  await act(async () => first.resolve(catalog("first")));
  expect(screen.getByRole("option", { name: /second.*Configured/ })).toBeTruthy();
});
