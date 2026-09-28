import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, expect, test, vi } from "vitest";
import { WorkspaceFilesPanel } from "../src/components/WorkspaceFilesPanel";
import { useWorkspacePanelController } from "../src/components/app/useWorkspacePanelController";
import { getWorkspaceFileTree, readDesktopFilePreview } from "../src/lib/workspaceBridge";
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
vi.mock("../src/lib/workspaceBridge", () => ({ getWorkspaceFileTree: vi.fn(), readDesktopFilePreview: vi.fn() }));
let view: ReactTestRenderer;
afterEach(async () => { if (view) await act(async () => view.unmount()); vi.unstubAllGlobals(); vi.resetAllMocks(); });
test("file tree has a working refresh instead of an inert scope selector", async () => {
  vi.mocked(getWorkspaceFileTree).mockResolvedValue({ root: "root", entries: [] } as never);
  await act(async () => { view = create(<WorkspaceFilesPanel isOpen workspaceRoot="root" />); });
  expect(view.root.findAllByProps({ className: "workspaceFilesScope" }).filter(n => n.type === "button")).toHaveLength(0);
  await act(async () => view.root.findByProps({ "aria-label": "Refresh files" }).props.onClick());
  expect(getWorkspaceFileTree).toHaveBeenCalledTimes(2);
});
test("explicit refresh updates content without adding or reordering tabs", async () => {
  let panel!: ReturnType<typeof useWorkspacePanelController>;
  function Harness() { panel = useWorkspacePanelController({ workspaceRoot: "root", sessions: [], currentSessionId: null }); return null; }
  vi.mocked(readDesktopFilePreview).mockImplementation(async (path) => ({ path, name: path, content: "old", contentKind: "text" }) as never);
  await act(async () => { view = create(<Harness />); });
  await act(async () => { await panel.actions.openFilePath("a.md"); });
  vi.mocked(readDesktopFilePreview).mockImplementation(async (path) => ({ path, name: path, content: "new", contentKind: "text" }) as never);
  await act(async () => { await panel.actions.refreshActiveFile(); });
  expect(panel.tabs[0].content).toBe("new");
  const ids = panel.tabs.map(t => t.id);
  await act(async () => { await panel.actions.refreshActiveFile(); });
  expect(panel.tabs.map(t => t.id)).toEqual(ids);
  expect(readDesktopFilePreview).toHaveBeenCalledTimes(3);
});

test("window focus refreshes the active file without changing tab identity", async () => {
  const windowEvents = new EventTarget();
  const documentEvents = Object.assign(new EventTarget(), { visibilityState: "visible" });
  vi.stubGlobal("window", windowEvents); vi.stubGlobal("document", documentEvents);
  let panel!: ReturnType<typeof useWorkspacePanelController>;
  function Harness() { panel = useWorkspacePanelController({ workspaceRoot: "root", sessions: [], currentSessionId: null }); return null; }
  vi.mocked(readDesktopFilePreview).mockImplementation(async (path) => ({ path, name: path, content: "old", contentKind: "text" }) as never);
  await act(async () => { view = create(<Harness />); });
  await act(async () => { await panel.actions.openFilePath("a.md"); });
  const id = panel.activeTabId;
  vi.mocked(readDesktopFilePreview).mockImplementation(async (path) => ({ path, name: path, content: "updated", contentKind: "text" }) as never);
  await act(async () => { windowEvents.dispatchEvent(new Event("focus")); });
  expect(panel.tabs[0].content).toBe("updated"); expect(panel.activeTabId).toBe(id);
  await act(async () => panel.actions.collapse());
  await act(async () => { windowEvents.dispatchEvent(new Event("focus")); });
  expect(readDesktopFilePreview).toHaveBeenCalledTimes(2);
});

test("returning to a saved session restores its file tabs", async () => {
  let panel!: ReturnType<typeof useWorkspacePanelController>;
  function Harness() { panel = useWorkspacePanelController({ workspaceRoot: "root", sessions: [], currentSessionId: null }); return null; }
  vi.mocked(readDesktopFilePreview).mockImplementation(async path => ({ path, name: path, content: path, contentKind: "text" }) as never);
  await act(async () => { view = create(<Harness />); });
  await act(async () => { await panel.actions.openFilePath("a.md"); });
  await act(async () => { panel.actions.saveScope("a"); panel.actions.restoreScope("b"); });
  expect(panel.tabs).toHaveLength(0);
  await act(async () => { await panel.actions.openFilePath("b.md"); });
  await act(async () => { panel.actions.saveScope("b"); panel.actions.restoreScope("a"); });
  expect(panel.tabs.map(t => t.path)).toEqual(["a.md"]);
});
