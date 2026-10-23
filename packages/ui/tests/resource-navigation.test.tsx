// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { PluginsDialog } from "../src/components/PluginsDialog";
import { useAppNavigation, type AppLocation } from "../src/components/app/useAppNavigation";
import { SkillsDialog } from "../src/components/SkillsDialog";
import { getPluginDetail, getSkillCatalog, getSkillDetail, setPluginEnabled, reloadPlugins, type PluginDescriptorV1, type SkillEntry } from "../src/lib/chatBridge";
function plugin(): PluginDescriptorV1 { return { id: "p", name: "Example", description: "Plugin description", source: "managed", enabled: true, errors: [], tools: [], path: "/p", scopes: ["user"], activationStatus: "active", policySource: "user" }; }
function skill(id: string, scope: SkillEntry["scope"] = "user"): SkillEntry { return { skillId: id, sourceId: id, scope, name: id === "s" ? "Design" : id, description: id === "s" ? "Design guidance" : "", enabled: true, errors: [], allowImplicitInvocation: true, capabilityMetadata: { allowedTools: [] }, skillMdPath: `/skills/${id}/SKILL.md`, rootPath: `/skills/${id}`, contentHash: "hash" }; }
vi.mock("../src/lib/chatBridge", () => ({
 listPlugins: vi.fn(async () => [plugin()]),
 getPluginDetail: vi.fn(async () => ({ descriptor: plugin(), capabilities: { skills: [], cli: [], apps: [], hooks: [], mcpServers: [], capabilities: [] } })),
 setPluginEnabled: vi.fn(async () => ({})), reloadPlugins: vi.fn(async () => ({})),
 listSkillSources: vi.fn(async () => ({ schemaVersion: "skill.sources.v1", sources: [], skillPolicies: [] })),
 getSkillCatalog: vi.fn(async () => ({ schema: "skill_catalog_snapshot_v1", cwd: "/project", catalogHash: "hash", sources: [], skills: [skill("s"), ...(["workspace", "system", "plugin"] as const).map(scope => skill(scope, scope))], diagnostics: [] })),
 getSkillDetail: vi.fn(async () => ({ skill: skill("s"), content: "Design instructions from the selected skill" })),
}));
beforeEach(() => { vi.clearAllMocks(); });
afterEach(cleanup);

async function chooseScope(user: ReturnType<typeof userEvent.setup>, name: string) {
  await user.click(screen.getByRole("combobox", { name: "Skill scope" }));
  await user.click(await screen.findByRole("option", { name }));
}

test("plugin selection navigates to detail while list search survives return", async () => {
  const user = userEvent.setup(); render(<PluginsDialog />);
  await screen.findByRole("button", { name: "Open Example" });
  await user.type(screen.getByRole("textbox", { name: "Search plugins" }), "Example");
  await user.click(screen.getByRole("button", { name: "Open Example" }));
  expect(await screen.findByRole("heading", { name: "Example" })).toBeDefined();
  await user.click(screen.getByRole("button", { name: "Back to plugins" }));
  expect(await screen.findByRole("textbox", { name: "Search plugins" })).toHaveProperty("value", "Example");
});

test("plugin detail discarded after returning to the list cannot surface a late error", async () => {
  let reject!: (error: Error) => void;
  vi.mocked(getPluginDetail).mockImplementationOnce(() => new Promise((_resolve, fail) => { reject = fail; }));
  const user = userEvent.setup(); render(<PluginsDialog />);
  await user.click(await screen.findByRole("button", { name: "Open Example" }));
  await waitFor(() => expect(getPluginDetail).toHaveBeenCalledExactlyOnceWith({ id: "p" }));
  await user.click(screen.getByRole("button", { name: "Back to plugins" }));
  await act(async () => reject(new Error("obsolete detail error")));
  expect(screen.queryByText("obsolete detail error")).toBeNull();
  expect(screen.getByRole("textbox", { name: "Search plugins" })).toBeDefined();
});

test("plugin reload and enablement controls invoke the selected plugin operations", async () => {
  const user = userEvent.setup(); render(<PluginsDialog />);
  await screen.findByRole("button", { name: "Open Example" });
  await user.click(screen.getByRole("button", { name: "Reload plugins" }));
  await waitFor(() => expect(reloadPlugins).toHaveBeenCalledExactlyOnceWith());
  await user.click(screen.getByRole("button", { name: "Open Example" }));
  await user.click(await screen.findByRole("switch", { name: "Disable plugin" }));
  await waitFor(() => expect(setPluginEnabled).toHaveBeenCalledExactlyOnceWith({ id: "p", enabled: false }));
});

test("skill detail returns to the list with its query intact", async () => {
  const user = userEvent.setup(); render(<SkillsDialog confirmAction={async () => true} />);
  await screen.findByRole("button", { name: "Open Design" });
  await user.type(screen.getByRole("textbox", { name: "Search skills" }), "Design");
  await user.click(screen.getByRole("button", { name: "Open Design" }));
  expect(await screen.findByRole("heading", { name: "Design" })).toBeDefined();
  expect(await screen.findByText("Design instructions from the selected skill")).toBeDefined();
  expect(getSkillDetail).toHaveBeenCalledExactlyOnceWith({ cwd: undefined, skillId: "s" });
  await user.click(screen.getByRole("button", { name: "Back to skills" }));
  expect(screen.getByRole("textbox", { name: "Search skills" })).toHaveProperty("value", "Design");
});

test("global back and forward restore the plugin detail with the list query intact", async () => {
  function Harness() {
    const [location, setLocation] = useState<AppLocation>({ page: "plugins", sessionId: null, root: null, pluginId: "" });
    const nav = useAppNavigation(location, async next => { setLocation(next); return true; });
    return <><button disabled={!nav.canBack} onClick={() => { void nav.move(-1); }}>Navigate back</button><button disabled={!nav.canForward} onClick={() => { void nav.move(1); }}>Navigate forward</button><PluginsDialog selectedPluginId={location.pluginId} onSelect={pluginId => { void nav.move({ ...location, pluginId }); }} /></>;
  }
  const user = userEvent.setup(); render(<Harness />);
  await screen.findByRole("button", { name: "Open Example" });
  await user.type(screen.getByRole("textbox", { name: "Search plugins" }), "Example");
  await user.click(screen.getByRole("button", { name: "Open Example" }));
  await screen.findByRole("heading", { name: "Example" });
  await user.click(screen.getByRole("button", { name: "Navigate back" }));
  expect(await screen.findByRole("textbox", { name: "Search plugins" })).toHaveProperty("value", "Example");
  await user.click(screen.getByRole("button", { name: "Navigate forward" }));
  expect(await screen.findByRole("heading", { name: "Example" })).toBeDefined();
});

test("skill scope and query survive detail navigation", async () => {
  const user = userEvent.setup(); render(<SkillsDialog confirmAction={async () => true} />);
  await screen.findByRole("button", { name: "Open Design" });
  await chooseScope(user, "System");
  expect(screen.queryByRole("button", { name: "Open Design" })).toBeNull();
  expect(screen.getByRole("button", { name: "Open system" })).toBeDefined();
  await chooseScope(user, "Personal");
  await user.type(screen.getByRole("textbox", { name: "Search skills" }), "Design");
  await user.click(screen.getByRole("button", { name: "Open Design" }));
  await screen.findByRole("heading", { name: "Design" });
  await user.click(screen.getByRole("button", { name: "Back to skills" }));
  expect(screen.getByRole("textbox", { name: "Search skills" })).toHaveProperty("value", "Design");
  expect(screen.getByRole("combobox", { name: "Skill scope" }).textContent).toContain("Personal");
});

test("Skills defaults to All sources and keeps explicit Personal and System scopes", async () => {
  const user = userEvent.setup(); render(<SkillsDialog workspaceRoot="/project" confirmAction={async () => true} />);
  await screen.findByRole("button", { name: "Open Design" });
  expect(getSkillCatalog).toHaveBeenCalledExactlyOnceWith("/project");
  const filter = screen.getByRole("combobox", { name: "Skill scope" });
  expect(filter.textContent).toContain("All sources");
  await user.click(filter);
  expect(await screen.findAllByRole("option")).toHaveLength(3);
  expect(screen.getByRole("option", { name: "All sources" })).toBeDefined();
  expect(screen.getByRole("option", { name: "Personal" })).toBeDefined();
  expect(screen.getByRole("option", { name: "System" })).toBeDefined();
  await user.keyboard("{Escape}");
  expect(screen.getByRole("button", { name: "Open workspace" })).toBeDefined();
  expect(screen.getByRole("button", { name: "Open system" })).toBeDefined();
  expect(screen.queryByRole("button", { name: "Open plugin" })).toBeNull();
  await chooseScope(user, "System");
  expect(screen.getByRole("button", { name: "Open system" })).toBeDefined();
  for (const name of ["Design", "workspace", "plugin"]) expect(screen.queryByRole("button", { name: `Open ${name}` })).toBeNull();
});
