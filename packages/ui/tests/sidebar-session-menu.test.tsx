import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, test, vi } from "vitest";
import { Sidebar } from "../src/components/Sidebar";
import { ConfirmDialog } from "../src/components/ConfirmDialog";
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
test("modal confirmation opens and closes and supports buttons, Escape, and backdrop cancellation", async () => {
  const onCancel = vi.fn(), onConfirm = vi.fn();
  const node = {open:false,showModal:vi.fn(),close:vi.fn()};
  let view!: ReactTestRenderer;
  await act(async () => {view=create(<ConfirmDialog open title="Remove resource" message="Cannot be undone" onCancel={onCancel} onConfirm={onConfirm}/>,{createNodeMock:()=>node});});
  expect(node.showModal).toHaveBeenCalledOnce();
  const dialog = view.root.findByType("dialog");
  expect(dialog.props.role).toBe("alertdialog");
  expect(dialog.props["aria-labelledby"]).toBe(view.root.findByType("h1").props.id);
  expect(dialog.props["aria-describedby"]).toBe(view.root.findByType("p").props.id);
  const buttons=dialog.findAllByType("button");
  expect(buttons[0].props.autoFocus).toBe(true);
  buttons[0].props.onClick(); buttons[1].props.onClick();
  const preventDefault=vi.fn(); dialog.props.onCancel({preventDefault});
  expect(preventDefault).toHaveBeenCalledOnce();
  const currentTarget={getBoundingClientRect:()=>({left:10,right:100,top:10,bottom:100})};
  dialog.props.onClick({currentTarget,clientX:50,clientY:50});
  expect(onCancel).toHaveBeenCalledTimes(2);
  dialog.props.onClick({currentTarget,clientX:0,clientY:50});
  expect(onCancel).toHaveBeenCalledTimes(3);
  expect(onConfirm).toHaveBeenCalledOnce();
  node.open=true;
  await act(async () => view.update(<ConfirmDialog open={false} title="Remove resource" onCancel={onCancel} onConfirm={onConfirm}/>));
  expect(node.close).toHaveBeenCalledOnce();
  await act(async () => view.unmount());
});
