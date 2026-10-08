import { beforeEach, expect, test, vi } from "vitest";
const host = vi.hoisted(() => ({ invokeHost: vi.fn(), isNativeHostRuntime: vi.fn(() => true) }));
vi.mock("../src/host/hostBridge", () => ({ ...host, listenHost: vi.fn() }));
import { addSkillSource, getSkillCatalog, getSkillDetail } from "../src/lib/chatBridge";
beforeEach(() => { host.invokeHost.mockReset(); host.isNativeHostRuntime.mockReturnValue(true); });
test.each(["catalogDirectory", "skillFile"])("registers an explicit %s workspace location", async kind => {
  const request = { scope: "workspace", kind, path: "/skills", workspaceRoot: "/project" };
  const response = { sources: [], skillPolicies: [] };
  host.invokeHost.mockResolvedValue(response);
  expect(await addSkillSource(request)).toBe(response);
  expect(host.invokeHost).toHaveBeenCalledExactlyOnceWith("skill/source/add", {request});
});
test("personal sources and catalog/detail requests normalize omitted workspace context", async () => {
  await addSkillSource({scope:"user",kind:"skillFile",path:"/SKILL.md"});
  await getSkillCatalog();
  await getSkillDetail({skillId:"design"});
  expect(host.invokeHost.mock.calls).toEqual([
    ["skill/source/add", {request:{scope:"user",kind:"skillFile",path:"/SKILL.md",workspaceRoot:null}}],
    ["skill/catalog", {request:{cwd:null}}],
    ["skill/detail", {request:{cwd:null,skillId:"design"}}],
  ]);
});
test("skill access outside the native host fails before invocation", async () => {
  host.isNativeHostRuntime.mockReturnValue(false);
  for (const call of [() => addSkillSource({scope:"user",kind:"skillFile",path:"/SKILL.md"}), getSkillCatalog, () => getSkillDetail({skillId:"design"})]) await expect(call()).rejects.toThrow("desktop-only");
  expect(host.invokeHost).not.toHaveBeenCalled();
});
