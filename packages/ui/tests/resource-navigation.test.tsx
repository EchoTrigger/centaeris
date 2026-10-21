// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { PluginsDialog } from "../src/components/PluginsDialog";
import { useAppNavigation, type AppLocation } from "../src/components/app/useAppNavigation";
import { SkillsDialog } from "../src/components/SkillsDialog";
import { getPluginDetail, getSkillCatalog, getSkillDetail, setPluginEnabled, reloadPlugins } from "../src/lib/chatBridge";
vi.mock("../src/lib/chatBridge", () => ({
 listPlugins: vi.fn(async () => [{ id:"p",name:"Example",description:"Plugin description",source:"managed",enabled:true,errors:[],tools:[],path:"/p" }]),
 getPluginDetail: vi.fn(async () => ({descriptor:{id:"p"},capabilities:{skills:[],cli:[],apps:[],hooks:[],mcpServers:[],capabilities:[]}})),
 setPluginEnabled: vi.fn(async () => ({})),
 reloadPlugins: vi.fn(async () => ({})),
 listSkillSources: vi.fn(async () => ({sources:[],skillPolicies:[]})),
 getSkillCatalog: vi.fn(async () => ({skills:[{skillId:"s",sourceId:"src",scope:"user",name:"Design",description:"Design guidance",enabled:true,errors:[],capabilityMetadata:{allowedTools:[]}}, ...(["workspace", "system", "plugin"] as const).map(scope => ({skillId:scope,sourceId:scope,scope,name:scope,description:"",enabled:true,errors:[],capabilityMetadata:{allowedTools:[]}}))],diagnostics:[]})),
 getSkillDetail: vi.fn(async () => ({skill:{skillId:"s",scope:"user",name:"Design",description:"Design guidance",enabled:true,errors:[],capabilityMetadata:{allowedTools:[]}},content:"Instructions"})),
}));
beforeEach(() => { vi.clearAllMocks(); });
afterEach(cleanup);

test("plugin selection navigates to detail while list search survives return", async () => {
  const user = userEvent.setup();
  render(<PluginsDialog />);
  await screen.findByRole("button", { name: "Open Example" });
  await user.type(screen.getByRole("textbox", { name: "Search plugins" }), "Example");
  await user.click(screen.getByRole("button", { name: "Open Example" }));
  expect(await screen.findByRole("navigation", { name: "Plugin navigation" })).toBeDefined();
  await user.click(screen.getByRole("button", { name: "Back to plugins" }));
  expect((await screen.findByRole("textbox", { name: "Search plugins" }) as HTMLInputElement).value).toBe("Example");
});

test("plugin detail discarded after returning to the list cannot surface a late error", async () => {
  let reject!: (error: Error) => void;
  vi.mocked(getPluginDetail).mockImplementationOnce(() => new Promise((_resolve, fail) => { reject = fail; }));
  const user = userEvent.setup();
  render(<PluginsDialog />);
  await user.click(await screen.findByRole("button", { name: "Open Example" }));
  await waitFor(() => expect(getPluginDetail).toHaveBeenCalledExactlyOnceWith({ id: "p" }));
  await user.click(screen.getByRole("button", { name: "Back to plugins" }));
  await act(async () => reject(new Error("obsolete detail error")));
  expect(screen.queryByText("obsolete detail error")).toBeNull();
  expect(screen.getByRole("textbox", { name: "Search plugins" })).toBeDefined();
});

test("plugin reload and enablement controls invoke the selected plugin operations", async () => {
  const user = userEvent.setup();
  render(<PluginsDialog />);
  await screen.findByRole("button", { name: "Open Example" });
  await user.click(screen.getByRole("button", { name: "Reload plugins" }));
  await waitFor(() => expect(reloadPlugins).toHaveBeenCalledExactlyOnceWith());
  await user.click(screen.getByRole("button", { name: "Open Example" }));
  await user.click(await screen.findByRole("button", { name: "Disable plugin" }));
  await waitFor(() => expect(setPluginEnabled).toHaveBeenCalledExactlyOnceWith({ id: "p", enabled: false }));
});

test("skill opens in a dismissible dialog and preserves list query", async () => {
  const user = userEvent.setup();
  render(<SkillsDialog confirmAction={async () => true} />);
  await screen.findByRole("button", { name: "Open Design" });
  await user.type(screen.getByRole("textbox", { name: "Search skills" }), "Design");
  await user.click(screen.getByRole("button", { name: "Open Design" }));
  const detail = await screen.findByRole("dialog", { name: "Design" });
  expect(await within(detail).findByText("Instructions")).toBeDefined();
  expect(getSkillDetail).toHaveBeenCalledExactlyOnceWith({ cwd: undefined, skillId: "s" });
  await user.click(within(detail).getByRole("button", { name: "Close detail" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect((screen.getByRole("textbox", { name: "Search skills" }) as HTMLInputElement).value).toBe("Design");
});

test("global back and forward restore the plugin detail with the list query intact", async () => {
  function Harness() {
    const [location, setLocation] = useState<AppLocation>({ page: "plugins", sessionId: null, root: null, pluginId: "" });
    const nav = useAppNavigation(location, async next => { setLocation(next); return true; });
    return <>
      <button disabled={!nav.canBack} onClick={() => { void nav.move(-1); }}>Navigate back</button>
      <button disabled={!nav.canForward} onClick={() => { void nav.move(1); }}>Navigate forward</button>
      <PluginsDialog selectedPluginId={location.pluginId} onSelect={pluginId => { void nav.move({ ...location, pluginId }); }} />
    </>;
  }
  const user = userEvent.setup();
  render(<Harness />);
  await screen.findByRole("button", { name: "Open Example" });
  await user.type(screen.getByRole("textbox", { name: "Search plugins" }), "Example");
  await user.click(screen.getByRole("button", { name: "Open Example" }));
  await screen.findByRole("navigation", { name: "Plugin navigation" });
  await user.click(screen.getByRole("button", { name: "Navigate back" }));
  await waitFor(() => expect(screen.queryByRole("navigation", { name: "Plugin navigation" })).toBeNull());
  expect((screen.getByRole("textbox", { name: "Search plugins" }) as HTMLInputElement).value).toBe("Example");
  await user.click(screen.getByRole("button", { name: "Navigate forward" }));
  expect(await screen.findByRole("navigation", { name: "Plugin navigation" })).toBeDefined();
});

test("skill scope filters the catalog and Escape closes detail without resetting search", async () => {
  const user = userEvent.setup();
  render(<SkillsDialog confirmAction={async () => true} />);
  await screen.findByRole("button", { name: "Open Design" });
  const scope = within(screen.getByRole("group", { name: "Skill scope" }));
  await user.click(scope.getByRole("button", { name: "System" }));
  expect(screen.queryByRole("button", { name: "Open Design" })).toBeNull();
  expect(screen.getByRole("button", { name: "Open system" })).toBeDefined();
  await user.click(scope.getByRole("button", { name: "Personal" }));
  await user.type(screen.getByRole("textbox", { name: "Search skills" }), "Design");
  await user.click(screen.getByRole("button", { name: "Open Design" }));
  await screen.findByRole("dialog", { name: "Design" });
  await user.keyboard("{Escape}");
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect((screen.getByRole("textbox", { name: "Search skills" }) as HTMLInputElement).value).toBe("Design");
  expect(scope.getByRole("button", { name: "Personal", pressed: true })).toBeDefined();
});

test("Skill has only Personal and System; Personal includes workspace skills but never plugin skills", async () => {
  const user = userEvent.setup();
  render(<SkillsDialog workspaceRoot="/project" confirmAction={async () => true} />);
  await screen.findByRole("button", { name: "Open Design" });
  expect(getSkillCatalog).toHaveBeenCalledExactlyOnceWith("/project");
  const scope = within(screen.getByRole("group", { name: "Skill scope" }));
  expect(scope.getAllByRole("button").map(button => button.textContent)).toEqual(["Personal", "System"]);
  expect(scope.getByRole("button", { name: "Personal", pressed: true })).toBeDefined();
  expect(screen.getByRole("button", { name: "Open workspace" })).toBeDefined();
  expect(screen.queryByRole("button", { name: "Open system" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Open plugin" })).toBeNull();
  await user.click(scope.getByRole("button", { name: "System" }));
  expect(screen.getByRole("button", { name: "Open system" })).toBeDefined();
  for (const name of ["Design", "workspace", "plugin"]) expect(screen.queryByRole("button", { name: `Open ${name}` })).toBeNull();
});
