vi.mock("../src/components/ui/dropdown-menu", async () => import("./helpers/menuTestDouble"));
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, test, vi } from "vitest";
import { Sidebar } from "../src/components/Sidebar";
vi.mock("../src/components/WorkspaceFilesPanel", () => ({ WorkspaceFilesPanel: () => null }));
globalThis.IS_REACT_ACT_ENVIRONMENT = true;

async function sidebar(onDeleteSession = vi.fn(async () => {})) {
  let view!: ReactTestRenderer;
  const noop = () => {};
  await act(async () => { view = create(<Sidebar sessions={[{id:"s",title:"Original",cwd:"/test",sessionKind:"main",messageCount:1}]} currentSessionId="s" workspaces={[{root:"/test",name:"test",sortOrder:0,updatedAt:0}]} activeWorkspaceRoot={null} runningSessionIds={new Set(["s"])} completedSessionIds={new Set()} workspaceCatalogError={null} onNewChat={noop} onOpenWorkspace={noop} onSelectWorkspace={noop} onRetryWorkspaceCatalog={async()=>{}} onResetWorkspaceCatalog={async()=>{}} onSelectSession={noop} onRenameSession={async()=>{}} onDeleteSession={onDeleteSession} onOpenResource={noop} onOpenFile={noop}/>); });
  await act(async () => view.root.findByProps({"aria-label":"Delete Original"}).props.onClick());
  return { view, onDeleteSession };
}
test("a running session can be deleted only after inline confirmation", async () => {
  const {view,onDeleteSession} = await sidebar();
  expect(onDeleteSession).not.toHaveBeenCalled();
  const dialog = view.root.findByProps({role:"alertdialog"});
  expect(dialog.props["aria-label"]).toBe("Delete Original?");
  const buttons = dialog.findAllByType("button");
  expect(buttons[1].props.autoFocus).toBe(true);
  expect(buttons[0].props.disabled).toBe(false);
  await act(async () => buttons[0].props.onClick());
  expect(onDeleteSession).toHaveBeenCalledExactlyOnceWith("s");
  expect(view.root.findAllByProps({role:"alertdialog"})).toHaveLength(0);
  await act(async () => view.unmount());
});
test("Escape and leaving the confirmation cancel; internal focus movement retains it", async () => {
  const {view,onDeleteSession} = await sidebar();
  await act(async () => view.root.findByProps({role:"alertdialog"}).props.onBlur({currentTarget:{contains:()=>true},relatedTarget:{}}));
  expect(view.root.findAllByProps({role:"alertdialog"})).toHaveLength(1);
  await act(async () => view.root.findByProps({role:"alertdialog"}).props.onKeyDown({key:"Escape"}));
  expect(view.root.findAllByProps({role:"alertdialog"})).toHaveLength(0);
  await act(async () => view.root.findByProps({"aria-label":"Delete Original"}).props.onClick());
  await act(async () => view.root.findByProps({role:"alertdialog"}).props.onBlur({currentTarget:{contains:()=>false},relatedTarget:null}));
  expect(view.root.findAllByProps({role:"alertdialog"})).toHaveLength(0);
  expect(onDeleteSession).not.toHaveBeenCalled();
  await act(async () => view.unmount());
});
