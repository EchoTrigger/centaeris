import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { beforeEach, expect, test, vi } from "vitest";
import { WorkspaceReview } from "../src/components/WorkspaceReview";
import {
  getWorkspaceGitView,
  stageWorkspaceGitFile,
  unstageWorkspaceGitFile,
  type WorkspaceGitView,
} from "../src/lib/workspaceBridge";
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
vi.mock("../src/lib/workspaceBridge", () => ({
  getWorkspaceGitView: vi.fn(),
  stageWorkspaceGitFile: vi.fn(),
  unstageWorkspaceGitFile: vi.fn(),
}));
vi.mock("../src/components/CodePreview", () => ({
  default: ({ content, targetLine }: { content: string; targetLine?: number }) => (
    <pre data-line={targetLine}>{content}</pre>
  ),
}));
const file = { path: "a.ts", originalPath: null, status: "M", added: 2, removed: 1 };
const state: WorkspaceGitView = {
  workspaceRoot: "root",
  branch: "main",
  source: "unstaged",
  snapshot: { head: "head", indexFingerprint: "index" },
  baseRef: null,
  files: [file, { ...file, path: "b.ts" }],
  hasConflicts: false,
  diff: null,
};
const button = (view: ReactTestRenderer, label: string) =>
  view.root
    .findAllByType("button")
    .find((b) => b.props["aria-label"] === label || b.children.includes(label))!;
const input = (view: ReactTestRenderer, label: string) =>
  view.root.findByProps({ "aria-label": label });
const response = (path: string, text = "+needle\n context\n+needle") => ({
  ...state,
  diff: { workspaceRoot: "root", path, diffPreview: text, truncated: false },
});
beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(getWorkspaceGitView).mockImplementation(async (_root, source, _base, path) => ({
    ...state,
    source,
    ...(path ? { diff: response(path).diff } : {}),
  }));
});

test("source selection, collapsed file rows, statistics and no commit form", async () => {
  let view!: ReactTestRenderer;
  await act(async () => {
    view = create(<WorkspaceReview root="root" />);
  });
  expect(getWorkspaceGitView).toHaveBeenCalledExactlyOnceWith("root", "unstaged", undefined);
  expect(view.root.findAllByType("pre")).toHaveLength(0);
  expect(view.root.findAllByType("textarea")).toHaveLength(0);
  expect(button(view, "Expand a.ts").props["aria-expanded"]).toBe(false);
  expect(view.root.findAllByType("span").some((node) => node.children.join("") === "+2")).toBe(
    true,
  );
  await act(async () =>
    input(view, "Change source").props.onChange({ target: { value: "staged" } }),
  );
  await act(async () => button(view, "Expand a.ts").props.onClick());
  expect(getWorkspaceGitView).toHaveBeenLastCalledWith("root", "staged", undefined, "a.ts");
  expect(view.root.findByType("details").props.className).toBe("workspaceReviewMenu");
  await act(async () => button(view, "Unstage a.ts").props.onClick());
  expect(unstageWorkspaceGitFile).toHaveBeenCalledExactlyOnceWith("root", "a.ts", state.snapshot);
  await act(async () => view.unmount());
});

test("late diff cannot replace another expanded file, and truncation and opening stay explicit", async () => {
  const pending = new Map<string, (value: WorkspaceGitView) => void>();
  vi.mocked(getWorkspaceGitView).mockImplementation((_root, _source, _base, path) =>
    path ? new Promise((resolve) => pending.set(path, resolve)) : Promise.resolve(state),
  );
  const open = vi.fn();
  let view!: ReactTestRenderer;
  await act(async () => {
    view = create(<WorkspaceReview root="root" onOpenFile={open} />);
  });
  await act(async () => button(view, "Expand a.ts").props.onClick());
  await act(async () => button(view, "Expand b.ts").props.onClick());
  await act(async () =>
    pending.get("b.ts")!({
      ...response("b.ts", "+B"),
      diff: { ...response("b.ts", "+B").diff!, truncated: true },
    }),
  );
  await act(async () => pending.get("a.ts")!(response("a.ts", "+A")));
  expect(view.root.findByType("pre").children).toEqual(["+B"]);
  expect(JSON.stringify(view.toJSON())).toContain("Diff truncated");
  await act(async () => button(view, "Open file").props.onClick());
  expect(open).toHaveBeenCalledWith("b.ts");
  await act(async () => button(view, "Collapse b.ts").props.onClick());
  expect(view.root.findAllByType("pre")).toHaveLength(0);
  await act(async () => view.unmount());
});

test("file filtering and current diff match navigation", async () => {
  let view!: ReactTestRenderer;
  await act(async () => {
    view = create(<WorkspaceReview root="root" />);
  });
  await act(async () => input(view, "Filter files").props.onChange({ target: { value: "a.ts" } }));
  expect(button(view, "Expand b.ts")).toBeUndefined();
  await act(async () => button(view, "Expand a.ts").props.onClick());
  await act(async () =>
    input(view, "Find in diff").props.onChange({ target: { value: "needle" } }),
  );
  expect(view.root.findByType("pre").props["data-line"]).toBe(1);
  await act(async () => button(view, "Next match").props.onClick());
  expect(view.root.findByType("pre").props["data-line"]).toBe(3);
  await act(async () => view.unmount());
});

test("branch comparison applies a base and offers no staging action", async () => {
  let view!: ReactTestRenderer;
  await act(async () => {
    view = create(<WorkspaceReview root="root" />);
  });
  await act(async () =>
    input(view, "Change source").props.onChange({ target: { value: "branch" } }),
  );
  await act(async () =>
    input(view, "Base reference").props.onChange({ target: { value: "origin/develop" } }),
  );
  await act(async () => button(view, "Compare").props.onClick());
  expect(getWorkspaceGitView).toHaveBeenLastCalledWith("root", "branch", "origin/develop");
  await act(async () => button(view, "Expand a.ts").props.onClick());
  expect(button(view, "Stage a.ts")).toBeUndefined();
  await act(async () => view.unmount());
});

test("failed writes reconcile once; conflicts disable writes", async () => {
  vi.mocked(getWorkspaceGitView).mockResolvedValue({ ...state, hasConflicts: true });
  let view!: ReactTestRenderer;
  await act(async () => {
    view = create(<WorkspaceReview root="root" />);
  });
  await act(async () => button(view, "Expand a.ts").props.onClick());
  expect(button(view, "Stage a.ts").props.disabled).toBe(true);
  vi.mocked(getWorkspaceGitView).mockImplementation(async (_r, _s, _b, path) =>
    path ? response(path) : state,
  );
  await act(async () => button(view, "Refresh").props.onClick());
  vi.mocked(stageWorkspaceGitFile).mockRejectedValue(new Error("snapshot changed"));
  await act(async () => button(view, "Stage a.ts").props.onClick());
  expect(stageWorkspaceGitFile).toHaveBeenCalledTimes(1);
  expect(JSON.stringify(view.toJSON())).toContain("snapshot changed");
  await act(async () => view.unmount());
});

test("failed view can refresh and untracked file opens without an invented diff", async () => {
  vi.mocked(getWorkspaceGitView)
    .mockRejectedValueOnce(new Error("offline"))
    .mockResolvedValue({ ...state, files: [{ ...file, status: "?", added: null, removed: null }] });
  let view!: ReactTestRenderer;
  await act(async () => {
    view = create(<WorkspaceReview root="root" />);
  });
  expect(JSON.stringify(view.toJSON())).toContain("offline");
  await act(async () => button(view, "Refresh").props.onClick());
  await act(async () => button(view, "Expand a.ts").props.onClick());
  expect(getWorkspaceGitView).toHaveBeenCalledTimes(2);
  expect(JSON.stringify(view.toJSON())).toContain("Untracked file");
  await act(async () => view.unmount());
});

test("visible review refreshes on focus and interval, and stops polling when hidden or unmounted", async () => {
  vi.useFakeTimers();
  const win = Object.assign(new EventTarget(), {
    setInterval: globalThis.setInterval,
    clearInterval: globalThis.clearInterval,
  });
  const doc = Object.assign(new EventTarget(), { visibilityState: "visible" });
  vi.stubGlobal("window", win);
  vi.stubGlobal("document", doc);
  let view: ReactTestRenderer | undefined;
  try {
    await act(async () => {
      view = create(<WorkspaceReview root="root" />, {
        createNodeMock: () => ({ getClientRects: () => [{}] }),
      });
    });
    await act(async () => {
      win.dispatchEvent(new Event("focus"));
    });
    expect(getWorkspaceGitView).toHaveBeenCalledTimes(2);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(60_000);
    });
    expect(getWorkspaceGitView).toHaveBeenCalledTimes(3);
    doc.visibilityState = "hidden";
    await act(async () => {
      await vi.advanceTimersByTimeAsync(60_000);
    });
    expect(getWorkspaceGitView).toHaveBeenCalledTimes(3);
    await act(async () => view!.unmount());
    view = undefined;
    expect(vi.getTimerCount()).toBe(0);
  } finally {
    if (view) await act(async () => view!.unmount());
    vi.unstubAllGlobals();
    vi.useRealTimers();
  }
});
