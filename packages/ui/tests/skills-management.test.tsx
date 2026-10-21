// @vitest-environment jsdom
import { act } from "react";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import * as bridge from "../src/lib/chatBridge";
import { SkillsDialog } from "../src/components/SkillsDialog";
vi.mock("../src/lib/chatBridge", () => ({ listSkillSources: vi.fn(), getSkillCatalog: vi.fn(), reloadSkillCatalog: vi.fn(), getSkillDetail: vi.fn(), addSkillSource: vi.fn(), setSkillEnabled: vi.fn(), setSkillSourceEnabled: vi.fn(), revealSkillSource: vi.fn(), removeSkillSource: vi.fn(), selectSkillSourcePath: vi.fn() }));
vi.mock("../src/components/chat/MarkdownContent", () => ({ MarkdownContent: ({ text }: { text: string }) => <p>{text}</p> }));
const source = (id: string, scope: bridge.SkillSourceConfig["scope"] = "user", workspaceRoot: string | null = null): bridge.SkillSourceConfig => ({ sourceId: id, scope, kind: "catalogDirectory", path: `/skills/${id}`, enabled: true, workspaceRoot });
const skill = (id: string, scope: bridge.SkillEntry["scope"] = "user"): bridge.SkillEntry => ({ skillId: id, sourceId: id, name: `${id} skill`, description: `Instructions for ${id}`, scope, enabled: true, allowImplicitInvocation: true, capabilityMetadata: { allowedTools: ["read"] }, skillMdPath: `/skills/${id}/SKILL.md`, rootPath: `/skills/${id}`, contentHash: "hash", errors: [] });
const sources = (items = [source("personal"), source("secondary"), source("workspace", "workspace", "/project"), source("system", "system"), source("plugin", "plugin"), source("other-workspace", "workspace", "/other")]): bridge.SkillSourcesConfig => ({ schemaVersion: "skill.sources.v1", sources: items, skillPolicies: [] });
const catalog = (items = [skill("personal"), skill("workspace", "workspace"), skill("plugin", "plugin"), skill("system", "system")], cwd = "/project"): bridge.SkillCatalogSnapshot => ({ schema: "skill_catalog_snapshot_v1", cwd, catalogHash: "catalog", sources: sources().sources, skills: items, diagnostics: [] });
const confirm = vi.fn(async () => true);
beforeEach(() => { Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true }); });
beforeEach(() => {
  vi.mocked(bridge.listSkillSources).mockResolvedValue(sources());
  vi.mocked(bridge.getSkillCatalog).mockResolvedValue(catalog());
  vi.mocked(bridge.reloadSkillCatalog).mockResolvedValue(catalog());
  vi.mocked(bridge.getSkillDetail).mockImplementation(async ({ skillId }) => ({ skill: skill(skillId, skillId === "plugin" ? "plugin" : skillId === "system" ? "system" : skillId === "workspace" ? "workspace" : "user"), content: `${skillId} instructions` }));
  vi.mocked(bridge.setSkillEnabled).mockResolvedValue(catalog([skill("personal")]));
  vi.mocked(bridge.setSkillSourceEnabled).mockResolvedValue(sources());
  vi.mocked(bridge.selectSkillSourcePath).mockResolvedValue({ cancelled: true, path: null });
  confirm.mockResolvedValue(true);
});
afterEach(() => { cleanup(); vi.resetAllMocks(); });

test("Personal includes current-workspace skills, System is separate, and owned locations stay outside Skills management", async () => {
  const user = userEvent.setup(); render(<SkillsDialog workspaceRoot="/project" confirmAction={confirm} />);
  await screen.findByRole("button", { name: "Open personal skill" });
  expect(screen.getByRole("button", { name: "Open workspace skill" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Open system skill" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Open plugin skill" })).toBeNull();
  expect(within(screen.getByRole("combobox", { name: "Skill scope" })).getByText("Personal")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Open personal skill" }));
  await screen.findByText("personal instructions");
  await user.click(screen.getByRole("button", { name: "Manage location" }));
  expect(screen.getByRole("button", { name: "Remove location" })).toBeTruthy();
  expect(screen.getByText("/skills/personal")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Back to locations" }));
  expect(screen.getByRole("button", { name: "Open location workspace" })).toBeTruthy();
  for (const name of ["plugin", "system", "other-workspace"]) expect(screen.queryByRole("button", { name: `Open location ${name}` })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Back to skills" }));
  await user.click(screen.getByRole("combobox", { name: "Skill scope" }));
  expect((await screen.findAllByRole("option")).map(option => option.textContent)).toEqual(["Personal", "System"]);
  await user.click(screen.getByRole("option", { name: "System" }));
  expect(screen.getByRole("button", { name: "Open system skill" })).toBeTruthy();
  for (const name of ["personal", "workspace", "plugin"]) expect(screen.queryByRole("button", { name: `Open ${name} skill` })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Open system skill" }));
  await screen.findByText("system instructions");
  expect(screen.queryByRole("button", { name: "Manage location" })).toBeNull();
  expect(screen.getByRole("switch", { name: "Disable skill" })).toBeTruthy();
});

test("clearing skill filters restores the Personal view", async () => {
  const user = userEvent.setup(); render(<SkillsDialog workspaceRoot="/project" confirmAction={confirm} />);
  await screen.findByRole("button", { name: "Open personal skill" });
  await user.click(screen.getByRole("combobox", { name: "Skill scope" }));
  await user.click(screen.getByRole("option", { name: "System" }));
  await user.type(screen.getByRole("textbox", { name: "Search skills" }), "missing");
  await user.click(screen.getByRole("button", { name: "Clear filters" }));
  expect(within(screen.getByRole("combobox", { name: "Skill scope" })).getByText("Personal")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Open personal skill" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Open workspace skill" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Open plugin skill" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Open system skill" })).toBeNull();
});

test("skill mutation responses from a departed workspace cannot overwrite the new workspace", async () => {
  let finish!: (catalog: bridge.SkillCatalogSnapshot) => void;
  vi.mocked(bridge.setSkillEnabled).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const user = userEvent.setup();
  const { rerender } = render(<SkillsDialog workspaceRoot="/project" confirmAction={confirm} />);
  await user.click(await screen.findByRole("button", { name: "Open personal skill" }));
  await user.click(screen.getByRole("switch", { name: "Disable skill" }));
  vi.mocked(bridge.getSkillCatalog).mockResolvedValue(catalog([skill("new")], "/new"));
  rerender(<SkillsDialog workspaceRoot="/new" confirmAction={confirm} />);
  await screen.findByRole("button", { name: "Open new skill" });
  await act(async () => finish(catalog([skill("old")])));
  expect(screen.queryByRole("button", { name: "Open old skill" })).toBeNull();
  expect(screen.getByRole("button", { name: "Open new skill" })).toBeTruthy();
});

test("adding a location preserves dirty edits when cancelled, handles picker cancellation and confirms the exact scope", async () => {
  const user = userEvent.setup(); render(<SkillsDialog workspaceRoot="/project" confirmAction={confirm} />);
  await screen.findByRole("button", { name: "Open personal skill" });
  await user.click(screen.getByRole("button", { name: "Add location" }));
  const path = screen.getByRole("textbox", { name: "Path" });
  await user.type(path, "/custom/skills");
  confirm.mockResolvedValueOnce(false);
  await user.click(screen.getByRole("button", { name: "Cancel" }));
  expect(confirm).toHaveBeenCalledOnce(); expect(path).toHaveProperty("value", "/custom/skills");
  await user.click(screen.getByRole("button", { name: "Browse" }));
  expect(path).toHaveProperty("value", "/custom/skills");
  vi.mocked(bridge.addSkillSource).mockResolvedValue(sources([source("added", "workspace", "/project")]));
  vi.mocked(bridge.listSkillSources).mockResolvedValue(sources([source("added", "workspace", "/project")]));
  await user.click(screen.getByRole("button", { name: "Add location" }));
  await waitFor(() => expect(bridge.addSkillSource).toHaveBeenCalledExactlyOnceWith({ scope: "workspace", kind: "catalogDirectory", path: "/custom/skills", workspaceRoot: "/project" }));
  await screen.findByRole("heading", { name: "Skill locations" });
  expect(screen.getByText("Location added.")).toBeTruthy();
});

test("browse and content failures show an explicit retry without losing the current form or skill", async () => {
  const user = userEvent.setup(); render(<SkillsDialog workspaceRoot="/project" confirmAction={confirm} />);
  await screen.findByRole("button", { name: "Open personal skill" });
  await user.click(screen.getByRole("button", { name: "Add location" }));
  vi.mocked(bridge.selectSkillSourcePath).mockRejectedValueOnce(new Error("Picker unavailable"));
  await user.click(screen.getByRole("button", { name: "Browse" }));
  await screen.findByRole("alert");
  vi.mocked(bridge.selectSkillSourcePath).mockResolvedValue({ cancelled: false, path: "/chosen" });
  await user.click(screen.getByRole("button", { name: "Retry browse" }));
  await waitFor(() => expect(screen.getByRole("textbox", { name: "Path" })).toHaveProperty("value", "/chosen"));
  await user.click(screen.getByRole("button", { name: "Cancel" }));
  await screen.findByRole("heading", { name: "Skill locations" });
  await user.click(screen.getByRole("button", { name: "Back to skills" }));
  vi.mocked(bridge.getSkillDetail).mockRejectedValueOnce(new Error("Content unavailable"));
  await user.click(screen.getByRole("button", { name: "Open personal skill" }));
  await screen.findByRole("alert");
  await user.click(screen.getByRole("button", { name: "Retry" }));
  await screen.findByText("personal instructions");
});

test("invalid skills explain disabled controls and source removal requires approval", async () => {
  const invalid = { ...skill("invalid"), errors: ["SKILL.md is missing a name"] };
  vi.mocked(bridge.getSkillCatalog).mockResolvedValue(catalog([invalid]));
  const user = userEvent.setup(); render(<SkillsDialog workspaceRoot="/project" confirmAction={confirm} />);
  await user.click(await screen.findByRole("button", { name: "Open invalid skill" }));
  await user.click(screen.getByRole("switch", { name: "Disable skill" }));
  expect(bridge.setSkillEnabled).not.toHaveBeenCalled();
  expect(screen.getByRole("alert")).toHaveProperty("textContent", expect.stringContaining("missing a name"));
  await user.click(screen.getByRole("button", { name: "Back to skills" }));
  await user.click(screen.getByRole("button", { name: "Manage skill locations" }));
  await user.click(screen.getByRole("button", { name: "Open location personal" }));
  confirm.mockResolvedValueOnce(false);
  await user.click(screen.getByRole("button", { name: "Remove location" }));
  expect(bridge.removeSkillSource).not.toHaveBeenCalled();
});


test("location search explains empty matches, and skill back navigation returns to its location", async () => {
  const user = userEvent.setup(); render(<SkillsDialog workspaceRoot="/project" confirmAction={confirm} />);
  await user.click(await screen.findByRole("button", { name: "Manage skill locations" }));
  await screen.findByRole("button", { name: "Open location personal" });
  await user.type(screen.getByRole("textbox", { name: "Search skill locations" }), "missing");
  expect(screen.getByText("No matching locations")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Clear search" }));
  await user.click(screen.getByRole("button", { name: "Open location personal" }));
  await user.click(screen.getByRole("button", { name: "Open personal skill" }));
  await screen.findByText("personal instructions");
  await user.click(screen.getByRole("button", { name: "Back to location" }));
  expect(screen.getByRole("heading", { name: "personal" })).toBeTruthy();
});

test("a successful skill mutation remains authoritative over an older catalog refresh", async () => {
  let finish!: (value: bridge.SkillCatalogSnapshot) => void;
  vi.mocked(bridge.reloadSkillCatalog).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const user = userEvent.setup(); render(<SkillsDialog workspaceRoot="/project" confirmAction={confirm} />);
  await screen.findByRole("button", { name: "Open personal skill" });
  await user.click(screen.getByRole("button", { name: "Reload skills" }));
  await user.click(screen.getByRole("button", { name: "Open personal skill" }));
  vi.mocked(bridge.setSkillEnabled).mockResolvedValue(catalog([{ ...skill("personal"), enabled: false }]));
  await user.click(screen.getByRole("switch", { name: "Disable skill" }));
  await screen.findByRole("switch", { name: "Enable skill" });
  await act(async () => finish(catalog()));
  expect(screen.getByRole("switch", { name: "Enable skill" })).toBeTruthy();
});

test("location removal receipts update data without interrupting subsequent navigation", async () => {
  let finish!: (value: bridge.SkillSourcesConfig) => void;
  vi.mocked(bridge.removeSkillSource).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const user = userEvent.setup(); render(<SkillsDialog workspaceRoot="/project" confirmAction={confirm} />);
  await user.click(await screen.findByRole("button", { name: "Manage skill locations" }));
  await user.click(await screen.findByRole("button", { name: "Open location personal" }));
  await user.click(screen.getByRole("button", { name: "Remove location" }));
  await waitFor(() => expect(bridge.removeSkillSource).toHaveBeenCalledOnce());
  await user.click(screen.getByRole("button", { name: "Back to locations" }));
  await user.click(screen.getByRole("button", { name: "Open location secondary" }));
  vi.mocked(bridge.listSkillSources).mockResolvedValue(sources([source("secondary")]));
  await act(async () => finish(sources([source("secondary")])));
  expect(screen.getByRole("heading", { name: "secondary" })).toBeTruthy();
});

test("catalog diagnostics are visible even when a location cannot produce any skill rows", async () => {
  vi.mocked(bridge.getSkillCatalog).mockResolvedValue({ ...catalog([]), diagnostics: [{ code: "unavailable", message: "Personal location is unavailable", sourceId: "personal" }] });
  const user = userEvent.setup(); render(<SkillsDialog workspaceRoot="/project" confirmAction={confirm} />);
  await screen.findByText("Personal location is unavailable");
  await user.click(screen.getByRole("button", { name: "Review location" }));
  expect(screen.getByRole("heading", { name: "personal" })).toBeTruthy();
});


test("failed location addition retains input for retry and prevents duplicate submissions", async () => {
  let finish!: (value: bridge.SkillSourcesConfig) => void;
  vi.mocked(bridge.addSkillSource).mockRejectedValueOnce(new Error("Location unavailable")).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const user = userEvent.setup(); render(<SkillsDialog workspaceRoot="/project" confirmAction={confirm} />);
  await user.click(await screen.findByRole("button", { name: "Add location" }));
  await user.type(screen.getByRole("textbox", { name: "Path" }), "/retry/skills");
  await user.click(screen.getByRole("button", { name: "Add location" }));
  await screen.findByText("Location unavailable");
  expect(screen.getByRole("textbox", { name: "Path" })).toHaveProperty("value", "/retry/skills");
  await user.click(screen.getByRole("button", { name: "Add location" }));
  await user.click(screen.getByRole("button", { name: "Adding…" }));
  expect(bridge.addSkillSource).toHaveBeenCalledTimes(2);
  vi.mocked(bridge.listSkillSources).mockResolvedValue(sources([source("retry", "workspace", "/project")]));
  await act(async () => finish(sources([source("retry", "workspace", "/project")])));
  await screen.findByRole("button", { name: "Open location retry" });
});

test.each(["different-detail", "same-detail-again", "current-detail"] as const)("skill mutation errors respect the %s visit and leave retry available", async destination => {
  let reject!: (failure: Error) => void;
  vi.mocked(bridge.setSkillEnabled).mockImplementationOnce(() => new Promise((_resolve, fail) => { reject = fail; }));
  const user = userEvent.setup(); render(<SkillsDialog workspaceRoot="/project" confirmAction={confirm} />);
  await user.click(await screen.findByRole("button", { name: "Open personal skill" }));
  await user.click(screen.getByRole("switch", { name: "Disable skill" }));
  if (destination !== "current-detail") {
    await user.click(screen.getByRole("button", { name: "Back to skills" }));
    await user.click(screen.getByRole("button", { name: destination === "different-detail" ? "Open workspace skill" : "Open personal skill" }));
    await screen.findByRole("heading", { name: destination === "different-detail" ? "workspace skill" : "personal skill" });
  }
  await act(async () => reject(new Error("skill policy write unavailable")));
  if (destination === "current-detail") expect(screen.getByText("skill policy write unavailable")).toBeTruthy();
  else expect(screen.queryByText("skill policy write unavailable")).toBeNull();
  await user.click(screen.getByRole("switch"));
  expect(bridge.setSkillEnabled).toHaveBeenCalledTimes(2);
  expect(vi.mocked(bridge.setSkillEnabled).mock.calls[1][0]).toEqual({ cwd: "/project", skillId: destination === "different-detail" ? "workspace" : "personal", enabled: false });
});
