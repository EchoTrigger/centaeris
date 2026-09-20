import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, test, vi } from "vitest";
import { Sidebar } from "../src/components/Sidebar";

vi.mock("../src/components/WorkspaceFilesPanel", () => ({ WorkspaceFilesPanel: () => null }));
globalThis.IS_REACT_ACT_ENVIRONMENT = true;

async function editing(onRenameSession = vi.fn(async () => {})) {
  let renderer!: ReactTestRenderer;
  const noop = () => {};
  await act(async () => {
    renderer = create(<Sidebar sessions={[{ id: "s", title: "Original", cwd: "D:/test", sessionKind: "main", messageCount: 1 }]}
      currentSessionId="s" workspaces={[]} activeWorkspaceRoot={null} runningSessionIds={new Set()}
      completedSessionIds={new Set()} workspaceCatalogError={null} onNewChat={noop} onOpenWorkspace={noop}
      onSelectWorkspace={noop} onRetryWorkspaceCatalog={async () => {}} onResetWorkspaceCatalog={async () => {}}
      onSelectSession={noop} onRenameSession={onRenameSession} onDeleteSession={async () => {}}
      onOpenResource={noop} onOpenFile={noop} />);
  });
  await act(async () => renderer.root.findByProps({ "aria-label": "Rename Original" }).props.onClick());
  const input = () => renderer.root.findByType("input");
  await act(async () => input().props.onChange({ target: { value: "  Renamed  " } }));
  return { renderer, input, onRenameSession };
}

test("blur saves a trimmed title and exits editing", async () => {
  const f = await editing();
  await act(async () => f.input().props.onBlur());
  expect(f.onRenameSession).toHaveBeenCalledExactlyOnceWith("s", "Renamed");
  expect(f.renderer.root.findAllByType("input")).toHaveLength(0);
  await act(async () => f.renderer.unmount());
});

test("Enter followed by blur saves once; Escape followed by blur saves nothing", async () => {
  let finish!: () => void;
  const f = await editing(vi.fn(() => new Promise<void>((resolve) => { finish = resolve; })));
  const blur = f.input().props.onBlur;
  await act(async () => {
    f.renderer.root.findByType("form").props.onSubmit({ preventDefault() {} });
    blur();
  });
  expect(f.onRenameSession).toHaveBeenCalledTimes(1);
  await act(async () => finish());
  await act(async () => f.renderer.unmount());

  const cancelled = await editing();
  const cancelledBlur = cancelled.input().props.onBlur;
  await act(async () => {
    cancelled.input().props.onKeyDown({ key: "Escape", preventDefault() {} });
    cancelledBlur();
  });
  expect(cancelled.onRenameSession).not.toHaveBeenCalled();
  await act(async () => cancelled.renderer.unmount());
});

test("a blank name dismisses editing without replacing the original", async () => {
  const f = await editing();
  await act(async () => f.input().props.onChange({ target: { value: "  " } }));
  await act(async () => f.input().props.onBlur());
  expect(f.onRenameSession).not.toHaveBeenCalled();
  expect(f.renderer.root.findAllByType("input")).toHaveLength(0);
  await act(async () => f.renderer.unmount());
});
