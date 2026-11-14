// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { ApiError, apiJson } from "../../src/api";
import { i18n } from "../../src/i18n";
import PluginSettings from "../../src/routes/PluginSettings";

vi.mock("../../src/api", async original => ({ ...await original<typeof import("../../src/api")>(), apiJson: vi.fn() }));
const workspace = { id: "one", name: "Research", role: "owner" };
const catalog = { base: "/w/one/extensions", selectedName: "research", view: "connections", onSelect: vi.fn() };
const plugin = { name: "research", displayName: "Research", shortDescription: "Read sources", enabled: true, errors: [], mcpCredentialRefs: ["sources"], hooks: [], capabilities: ["mcp"],
  mcpServers: [{ id: "Sources", transport: { type: "streamableHttp", endpoint: "https://example.invalid/mcp" }, auth: { type: "bearer", credentialRef: "sources", credentialConfigured: false }, tools: [] }],
};
let credentialReads = 0;
let release: (() => void) | undefined;
beforeEach(async () => {
  credentialReads = 0; release = undefined; await i18n.changeLanguage("en");
  vi.mocked(apiJson).mockImplementation(async path => {
    if (path.endsWith("/plugins")) return { plugins: [plugin] };
    if (path.endsWith("/plugins/research")) return { plugin };
    if (path.endsWith("/mcp-bearer-credentials")) {
      if (++credentialReads === 1) throw new ApiError("connection_unavailable", 503);
      return new Promise(resolve => { release = () => resolve({ credentials: [] }); });
    }
    throw new Error(`Unexpected plugin request: ${path}`);
  });
});
afterEach(() => { cleanup(); vi.clearAllMocks(); });

test("retrying credential metadata preserves the user's unsubmitted token", async () => {
  const user = userEvent.setup();
  render(<MemoryRouter><PluginSettings workspace={workspace} isSuperuser={false} catalog={catalog} /></MemoryRouter>);
  await screen.findByRole("alert");
  const input = await screen.findByLabelText("sources Bearer Token");
  await user.type(input, "unsaved synthetic token");
  await user.click(screen.getByRole("button", { name: i18n.t("pluginSettings.reloadCredentials") }));
  await waitFor(() => expect(release).toBeTypeOf("function"));
  expect((screen.getByLabelText("sources Bearer Token") as HTMLInputElement).value).toBe("unsaved synthetic token");
  await act(async () => { release?.(); });
  expect((screen.getByLabelText("sources Bearer Token") as HTMLInputElement).value).toBe("unsaved synthetic token");
});

test("changing workspace clears credentials drafted for the previous workspace", async () => {
  const user = userEvent.setup();
  const view = render(<MemoryRouter><PluginSettings workspace={workspace} isSuperuser={false} catalog={catalog} /></MemoryRouter>);
  await screen.findByRole("alert");
  await user.type(await screen.findByLabelText("sources Bearer Token"), "unsaved synthetic token");
  view.rerender(<MemoryRouter><PluginSettings workspace={{ ...workspace, id: "two" }} isSuperuser={false} catalog={{ ...catalog, base: "/w/two/extensions" }} /></MemoryRouter>);
  await act(async () => { release?.(); });
  await waitFor(() => expect((screen.getByLabelText("sources Bearer Token") as HTMLInputElement).value).toBe(""));
});
