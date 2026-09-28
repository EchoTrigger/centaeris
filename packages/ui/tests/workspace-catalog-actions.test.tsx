import { act, create } from "react-test-renderer";
import { expect, test, vi } from "vitest";
import { useWorkspaceController } from "../src/components/app/useWorkspaceController";
import * as bridge from "../src/lib/workspaceBridge";
vi.mock("../src/lib/workspaceBridge", () => ({
  renameWorkspace: vi.fn(), removeWorkspace: vi.fn(), getWorkspaceInfo: vi.fn(),
  getWorkspaceGitStatus: vi.fn(async () => null), getWorkspaceGitHubCliStatus: vi.fn(async () => null),
}));
const snapshot = (name = "Project") => ({ cancelled: false, activeWorkspaceRoot: "D:/project", workspaces: [{ root: "D:/project", name, sortOrder: 0, updatedAt: 0 }] });
test("workspace actions persist display name and remove only the catalog entry", async () => {
  let controller!: ReturnType<typeof useWorkspaceController>;
  function Harness() { controller = useWorkspaceController({ confirmAction: async () => true, reportHostError: vi.fn() }); return null; }
  let view!: ReturnType<typeof create>;
  await act(async () => { view = create(<Harness />); });
  await act(async () => controller.actions.applySnapshot(snapshot()));
  vi.mocked(bridge.renameWorkspace).mockResolvedValue(snapshot("Named"));
  await act(async () => { expect(await controller.actions.renameWorkspace("D:/project", "Named")).toBe(true); });
  expect(controller.workspaces[0].name).toBe("Named");
  vi.mocked(bridge.removeWorkspace).mockResolvedValue({ removed: true });
  vi.mocked(bridge.getWorkspaceInfo).mockResolvedValue({ cancelled: false, activeWorkspaceRoot: null, workspaces: [] });
  await act(async () => { expect(await controller.actions.removeWorkspace("D:/project")).toBe(true); });
  expect(bridge.removeWorkspace).toHaveBeenCalledExactlyOnceWith("D:/project");
  expect(controller.workspaces).toEqual([]);
  await act(async () => view.unmount());
});
