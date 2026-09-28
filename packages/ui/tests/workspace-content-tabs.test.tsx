import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, test, vi } from "vitest";
import { SummaryPanel } from "../src/components/SummaryPanel";
import { useState } from "react";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

vi.mock("../src/components/CodePreview", () => ({ default: () => <pre /> }));
vi.mock("../src/components/WorkspaceFilesPanel", () => ({
  WorkspaceFilesPanel: ({ onOpenFile }: { onOpenFile: (entry: { path: string }) => void }) => {
    const [expanded, setExpanded] = useState(false);
    return (
      <div data-testid="file-tree">
        <button onClick={() => setExpanded(true)}>Expand</button>
        {expanded ? (
          <button onClick={() => onOpenFile({ path: "a.md" })}>Open nested file</button>
        ) : null}
      </div>
    );
  },
}));
vi.mock("../src/components/chat/AgentSessionPreview", () => ({
  AgentSessionPreview: ({ sessionId }: { sessionId: string }) => {
    const [page, setPage] = useState(1);
    return (
      <button data-session={sessionId} onClick={() => setPage(page + 1)}>
        {page}
      </button>
    );
  },
}));

test("content tabs have independent close controls and keyboard navigation", async () => {
  const select = vi.fn();
  const close = vi.fn();
  let view!: ReactTestRenderer;
  await act(async () => {
    view = create(
      <SummaryPanel
        tabs={[
          { id: "a", title: "a.md", kind: "file", path: "a.md", content: "A" },
          { id: "b", title: "b.md", kind: "file", path: "b.md", content: "B" },
        ]}
        activeTabId="a"
        onSelectTab={select}
        onCloseTab={close}
      />,
    );
  });
  const tabs = view.root.findAllByProps({ role: "tab" });
  expect(tabs).toHaveLength(2);
  expect(tabs[0].props["aria-selected"]).toBe(true);
  const focus = vi.fn();
  tabs[0].props.onKeyDown({
    key: "ArrowRight",
    preventDefault: vi.fn(),
    currentTarget: { closest: () => ({ querySelectorAll: () => [{ focus }, { focus }] }) },
  });
  expect(select).toHaveBeenCalledWith("b");
  const closeButton = view.root.findByProps({ "aria-label": "Close a.md" });
  expect(closeButton.type).toBe("button");
  expect(tabs[0].findAllByType("button").filter((button) => button !== tabs[0])).toHaveLength(0);
  await act(async () => closeButton.props.onClick());
  expect(close).toHaveBeenCalledWith("a");
  await act(async () => view.unmount());
});

test("the same file tree stays beside the preview when the first file replaces the Files landing tab", async () => {
  const open = vi.fn();
  let view!: ReactTestRenderer;
  await act(async () => {
    view = create(
      <SummaryPanel
        tabs={[{ id: "files", kind: "files", title: "Files", workspaceRoot: "root" }]}
        activeTabId="files"
        onSelectTab={vi.fn()}
        onCloseTab={vi.fn()}
        onOpenWorkspacePath={open}
      />,
    );
  });
  await act(async () =>
    view.root
      .findAllByType("button")
      .find((button) => button.children.includes("Expand"))!
      .props.onClick(),
  );
  await act(async () => {
    view.update(
      <SummaryPanel
        tabs={[
          {
            id: "a",
            kind: "file",
            title: "a.md",
            path: "a.md",
            workspaceRoot: "root",
            content: "A",
          },
        ]}
        activeTabId="a"
        onSelectTab={vi.fn()}
        onCloseTab={vi.fn()}
        onOpenWorkspacePath={open}
      />,
    );
  });
  expect(view.root.findAllByProps({ "data-testid": "file-tree" })).toHaveLength(1);
  await act(async () =>
    view.root
      .findAllByType("button")
      .find((button) => button.children.includes("Open nested file"))!
      .props.onClick(),
  );
  expect(open).toHaveBeenCalledWith("a.md");
  await act(async () => view.unmount());
});

test("switching content tabs preserves each Agent view and closing releases only that view", async () => {
  const tabs = [
    { id: "a", title: "Agent A", kind: "agent" as const, sessionId: "a" },
    { id: "b", title: "Agent B", kind: "agent" as const, sessionId: "b" },
  ];
  let view!: ReactTestRenderer;
  const render = (activeTabId: string, entries = tabs) => (
    <SummaryPanel
      tabs={entries}
      activeTabId={activeTabId}
      onSelectTab={vi.fn()}
      onCloseTab={vi.fn()}
    />
  );
  await act(async () => {
    view = create(render("a"));
  });
  await act(async () => view.root.findByProps({ "data-session": "a" }).props.onClick());
  await act(async () => view.update(render("b")));
  expect(view.root.findByProps({ "data-session": "a" }).children).toEqual(["2"]);
  expect(view.root.findByProps({ id: "panel-a" }).props.hidden).toBe(true);
  await act(async () => view.update(render("a")));
  expect(view.root.findByProps({ "data-session": "a" }).children).toEqual(["2"]);
  await act(async () => view.update(render("a", [tabs[0]])));
  expect(view.root.findAllByProps({ "data-session": "b" })).toHaveLength(0);
  expect(view.root.findByProps({ "data-session": "a" }).children).toEqual(["2"]);
  await act(async () => view.unmount());
});

vi.mock("../src/components/WorkspaceTasks", () => ({ WorkspaceTasks: ({ active }: { active: boolean }) => <div data-testid="task-observer" data-active={active} /> }));
test("Task observation follows tab selection and panel collapse", async () => {
  let view!: ReactTestRenderer;
  const render = (activeTabId: string, visible = true) => <SummaryPanel visible={visible} tabs={[{ id: "task", title: "Task", kind: "tasks", sessionId: "s1" }, { id: "file", title: "a.md", kind: "file", path: "a.md", content: "A" }]} activeTabId={activeTabId} onSelectTab={vi.fn()} onCloseTab={vi.fn()} />;
  await act(async () => { view = create(render("task")); });
  expect(view.root.findByProps({ "data-testid": "task-observer" }).props["data-active"]).toBe(true);
  await act(async () => view.update(render("file")));
  expect(view.root.findByProps({ "data-testid": "task-observer" }).props["data-active"]).toBe(false);
  await act(async () => view.update(render("task", false)));
  expect(view.root.findByProps({ "data-testid": "task-observer" }).props["data-active"]).toBe(false);
  await act(async () => view.unmount());
});
