import { act, create } from "react-test-renderer";
import { expect, test, vi } from "vitest";
import type { ComponentProps } from "react";
import { Sidebar } from "../src/components/Sidebar";

const noop = () => {};
const session = (id: string, cwd: string, updatedAt: number, isPinned = false) => ({ id, title: id, cwd, updatedAt, isPinned, sessionKind: "main" as const, messageCount: 1 });
const props: ComponentProps<typeof Sidebar> = {
  sessions: [session("old", "D:/A", 1), session("b", "D:/B", 2), session("new", "D:/A", 3), session("pin-a", "D:/A", 4, true), session("pin-b", "D:/B", 5, true)],
  workspaces: ["A", "B", "Empty"].map(name => ({ root: `D:/${name}`, name, sortOrder: 0, updatedAt: 0 })),
  currentSessionId: null, activeWorkspaceRoot: null, runningSessionIds: new Set(), completedSessionIds: new Set(), workspaceCatalogError: null,
  onNewChat: noop, onOpenWorkspace: noop, onSelectWorkspace: noop, onRetryWorkspaceCatalog: async () => {}, onResetWorkspaceCatalog: async () => {},
  onSelectSession: noop, onRenameSession: async () => {}, onDeleteSession: async () => {}, onOpenResource: noop, onOpenFile: noop,
};

test("Recents groups projects, sorts newest first and shows empty opened folders; Pinned stays global", async () => {
  let view!: ReturnType<typeof create>;
  await act(async () => { view = create(<Sidebar {...props} />); });
  const a = view.root.findByProps({ "aria-label": "Workspace D:/A" });
  expect(a.findAllByType("strong").map(node => node.children.join(""))).toEqual(["new", "old"]);
  expect(a.findByProps({ "aria-label": "Toggle A" }).findByType("svg").props.className).toContain("folder");
  expect(view.root.findByProps({ "aria-label": "Workspace D:/Empty" })).toBeDefined();
  expect(view.root.findByProps({ "aria-label": "Pinned" }).findAllByType("strong").map(node => node.children.join(""))).toEqual(["pin-b", "pin-a"]);
  await act(async () => view.unmount());
});

test("pinned rows support keyboard reorder across projects", async () => {
  const reorder = vi.fn(async () => {});
  let view!: ReturnType<typeof create>;
  await act(async () => { view = create(<Sidebar {...props} onReorderPinnedSessions={reorder} />); });
  await act(async () => view.root.findByProps({ "aria-label": "Reorder pin-a" }).props.onKeyDown({ key: "ArrowUp", preventDefault: noop }));
  expect(reorder).toHaveBeenCalledWith(["pin-a", "pin-b"]);
  await act(async () => view.unmount());
});

test("dropping a pinned handle below another row persists the complete new order", async () => {
  const reorder = vi.fn(async () => {});
  let view!: ReturnType<typeof create>;
  await act(async () => { view = create(<Sidebar {...props} onReorderPinnedSessions={reorder} />); });
  view.root.findByProps({ "aria-label": "Reorder pin-b" }).props.onDragStart({ dataTransfer: { setData: noop } });
  const row = view.root.findByProps({ "aria-label": "Reorder pin-a" }).parent!;
  await act(async () => row.props.onDrop({ preventDefault: noop, clientY: 40, currentTarget: { getBoundingClientRect: () => ({ top: 0, height: 45 }) } }));
  expect(reorder).toHaveBeenCalledWith(["pin-a", "pin-b"]);
  await act(async () => view.unmount());
});


test("workspace removal hides its group without removing sessions, and re-add restores them", async () => {
  let view!: ReturnType<typeof create>;
  await act(async () => { view = create(<Sidebar {...props} workspaces={props.workspaces.filter(w => w.name !== "A")} />); });
  expect(view.root.findAllByProps({ "aria-label": "Workspace D:/A" })).toHaveLength(0);
  expect(props.sessions.filter(s => s.cwd === "D:/A")).toHaveLength(3);
  await act(async () => view.update(<Sidebar {...props} />));
  expect(view.root.findByProps({ "aria-label": "Workspace D:/A" }).findAllByType("strong")).toHaveLength(2);
  await act(async () => view.unmount());
});

test("workspace disclosure and rename/remove actions are independent", async () => {
  const rename = vi.fn(async () => true);
  const remove = vi.fn(async () => true);
  let view!: ReturnType<typeof create>;
  await act(async () => { view = create(<Sidebar {...props} onRenameWorkspace={rename} onRemoveWorkspace={remove} />); });
  const group = () => view.root.findByProps({ "aria-label": "Workspace D:/A" });
  await act(async () => group().findByProps({ "aria-label": "Toggle A" }).props.onClick());
  expect(group().findAllByType("strong")).toHaveLength(0);
  await act(async () => group().findByProps({ "aria-label": "Toggle A" }).props.onClick());
  expect(group().findAllByType("strong")).toHaveLength(2);
  await act(async () => group().findAllByType("button").find(b => b.children.includes("Rename"))!.props.onClick());
  await act(async () => group().findByType("input").props.onChange({ target: { value: "Renamed" } }));
  await act(async () => group().findByType("form").props.onSubmit({ preventDefault: noop }));
  expect(rename).toHaveBeenCalledWith("D:/A", "Renamed");
  await act(async () => group().findAllByType("button").find(b => b.children.includes("Remove"))!.props.onClick());
  expect(remove).toHaveBeenCalledWith("D:/A");
  await act(async () => view.unmount());
});
