import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { beforeEach, expect, test, vi } from "vitest";
import type { DesktopFilePreviewReadResponse } from "../src/lib/workspaceBridge";
import type { SummaryPanelTab } from "../src/components/SummaryPanel";
import { getWorkspaceGitStatus } from "../src/lib/workspaceBridge";

type PreviewRequest = {
  path: string;
  promise: Promise<DesktopFilePreviewReadResponse>;
  resolve: (response: DesktopFilePreviewReadResponse) => void;
};

type ChatHarnessProps = {
  onOpenWorkspacePath: (
    path: string,
    options?: { startLine?: number; endLine?: number; taskId?: string },
  ) => Promise<void>;
  onOpenAgentSession: (sessionId: string, title: string) => void;
};

type SummaryHarnessProps = {
  tabs: SummaryPanelTab[];
  activeTabId: string | null;
  onCloseTab: (tabId: string) => void;
  onCollapse: () => void;
};

const harness = vi.hoisted(() => ({
  chatProps: null as ChatHarnessProps | null,
  summaryProps: null as SummaryHarnessProps | null,
  previewRequests: [] as PreviewRequest[],
}));

vi.mock("../src/components/Sidebar", () => ({
  Sidebar: () => <aside data-testid="sidebar" />,
}));

vi.mock("../src/components/chat/ChatArea", () => ({
  ChatArea: (props: ChatHarnessProps) => {
    harness.chatProps = props;
    return <main data-testid="chat-area" />;
  },
}));

vi.mock("../src/components/SummaryPanel", () => ({
  SummaryPanel: (props: SummaryHarnessProps) => {
    harness.summaryProps = props;
    return <section data-testid="summary-panel" />;
  },
}));

vi.mock("../src/components/ModelsDialog", () => ({
  ModelsDialog: () => null,
}));

vi.mock("../src/components/SkillsDialog", () => ({
  SkillsDialog: () => null,
}));

vi.mock("../src/components/PluginsDialog", () => ({
  PluginsDialog: () => null,
}));

vi.mock("../src/components/ConfirmDialog", () => ({
  ConfirmDialog: () => null,
}));

vi.mock("../src/lib/chatBridge", async () => {
 const { catalogFixture } = await import("./catalogFixture");
 const bridge = {
  activateSession: vi.fn(async () => undefined),
  deleteSession: vi.fn(),
  getAgentRuntimeConfig: vi.fn(async () => ({ selectableModels: [{ model: "test" }] })),
  listenAgentRuntimeConfigChanges: vi.fn(async () => () => undefined),
  listSessions: vi.fn(async () => []),
  updateSession: vi.fn(),
};
 return {...bridge, querySessionCatalog: catalogFixture(bridge.listSessions)};
});

vi.mock("../src/lib/workspaceBridge", () => ({
  activateWorkspaceRoot: vi.fn(),
  getWorkspaceGitHubCliStatus: vi.fn(async () => ({ available: false, summary: "" })),
  getWorkspaceGitStatus: vi.fn(async (workspaceRoot: string) => ({
    workspaceRoot,
    changedFiles: [],
    totalAdded: 0,
    totalRemoved: 0,
    isGitRepository: true,
  })),
  getWorkspaceInfo: vi.fn(async () => ({
    activeWorkspaceRoot: "D:\\Workspace",
    workspaces: [
      {
        root: "D:\\Workspace",
        name: "Workspace",
        sortOrder: 0,
        updatedAt: 1,
      },
    ],
    cancelled: false,
  })),
  openWorkspaceFolder: vi.fn(),
  readDesktopFilePreview: vi.fn((path: string) => {
    let resolveRequest: (response: DesktopFilePreviewReadResponse) => void = () => {};
    const promise = new Promise<DesktopFilePreviewReadResponse>((resolve) => {
      resolveRequest = resolve;
    });
    harness.previewRequests.push({
      path,
      promise,
      resolve: resolveRequest,
    });
    return promise;
  }),
  resetWorkspaceCatalog: vi.fn(),
}));

import App from "../src/App";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const getChatProps = (): ChatHarnessProps => {
  if (!harness.chatProps) throw new Error("ChatArea has not rendered");
  return harness.chatProps;
};

const getSummaryProps = (): SummaryHarnessProps => {
  if (!harness.summaryProps) throw new Error("SummaryPanel has not rendered");
  return harness.summaryProps;
};

const preview = (path: string, content: string): DesktopFilePreviewReadResponse => ({
  root: "D:\\Workspace",
  path,
  name: path,
  content,
  byteLen: content.length,
  encoding: "utf-8",
  contentKind: "text",
  mimeType: "text/plain",
});

beforeEach(() => {
  vi.stubGlobal("document", {
    documentElement: { dataset: {} },
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  });
  harness.chatProps = null;
  harness.summaryProps = null;
  harness.previewRequests.length = 0;
});

test("chat controls belong to the chat column beside the detail pane", async () => {
  let renderer!: ReactTestRenderer;
  await act(async () => { renderer = create(<App />); });
  const chat = renderer.root.findByProps({ className: "thinChatColumn" });
  expect(chat.findAllByProps({ "aria-label": "Workspace overview" })).toHaveLength(1);
  await act(async () => getChatProps().onOpenAgentSession("agent", "Agent"));
  expect(chat.findAllByProps({ "aria-label": "Workspace overview" })).toHaveLength(1);
  expect(chat.findAllByProps({ "aria-label": "Preview" })).toHaveLength(0);
  await act(async () => renderer.unmount());
});

test("workspace overview stays open when the user clicks outside", async () => {
  let renderer!: ReactTestRenderer;
  await act(async () => { renderer = create(<App />); });
  await act(async () => renderer.root.findByProps({ "aria-label": "Workspace overview" }).props.onClick());
  await act(async () => {
    for (const [type, listener] of vi.mocked(document.addEventListener).mock.calls) {
      if (type === "pointerdown" && typeof listener === "function") listener({ target: {} } as unknown as Event);
    }
  });
  expect(renderer.root.findByProps({ "aria-label": "Workspace overview" }).props["aria-expanded"]).toBe(true);
  await act(async () => renderer.unmount());
});

test("workspace split starts balanced, resizes by pointer and keyboard, and survives sidebar toggles", async () => {
  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(<App />);
  });
  await act(async () => getChatProps().onOpenAgentSession("agent-1", "Agent"));
  const handle = () => renderer.root.findByProps({ role: "separator" });
  expect(handle().props["aria-valuenow"]).toBe(50);
  const target = {
    setPointerCapture: vi.fn(),
    releasePointerCapture: vi.fn(),
    hasPointerCapture: () => true,
    parentElement: { getBoundingClientRect: () => ({ left: 200, width: 1000 }) },
  };
  await act(async () =>
    handle().props.onPointerDown({
      button: 0,
      pointerId: 1,
      preventDefault: vi.fn(),
      currentTarget: target,
    }),
  );
  await act(async () =>
    handle().props.onPointerMove({ pointerId: 1, clientX: 600, currentTarget: target }),
  );
  expect(handle().props["aria-valuenow"]).toBe(60);
  await act(async () => handle().props.onPointerUp({ pointerId: 1, currentTarget: target }));
  await act(async () =>
    renderer.root.findByProps({ "aria-label": "Hide left sidebar" }).props.onClick(),
  );
  expect(handle().props["aria-valuenow"]).toBe(60);
  await act(async () =>
    handle().props.onKeyDown({ key: "ArrowRight", preventDefault: vi.fn(), currentTarget: target }),
  );
  expect(handle().props["aria-valuenow"]).toBe(58);
  await act(async () => handle().props.onDoubleClick());
  expect(handle().props["aria-valuenow"]).toBe(50);
  await act(async () => renderer.unmount());
});

test("file browsing retains three files and replaces only the fourth preview slot", async () => {
  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(<App />);
  });
  await act(async () => {
    renderer.root.findByProps({ "aria-label": "Workspace overview" }).props.onClick();
  });
  await act(async () =>
    renderer.root.findByProps({ "aria-label": "Browse files" }).props.onClick(),
  );
  const operations: Promise<void>[] = [];
  await act(async () => {
    for (const path of ["a.md", "b.md", "c.md", "d.md", "e.md"]) {
      operations.push(getChatProps().onOpenWorkspacePath(path));
    }
  });
  expect(getSummaryProps().tabs.map((tab) => tab.title)).toEqual(["a.md", "b.md", "c.md", "e.md"]);
  expect(getSummaryProps().tabs.map((tab) => Boolean(tab.isPreview))).toEqual([
    false,
    false,
    false,
    true,
  ]);
  await act(async () => {
    for (const request of [...harness.previewRequests].reverse())
      request.resolve(preview(request.path, request.path));
    await Promise.all(operations);
  });
  expect(getSummaryProps().tabs.map((tab) => tab.content)).toEqual([
    "a.md",
    "b.md",
    "c.md",
    "e.md",
  ]);
  expect(getSummaryProps().tabs[3].isPreview).toBe(true);

  await act(async () => {
    await getChatProps().onOpenWorkspacePath("a.md");
  });
  expect(getSummaryProps().activeTabId).toBe(getSummaryProps().tabs[3].id);
  expect(getSummaryProps().tabs.map((tab) => tab.title)).toEqual(["e.md", "b.md", "c.md", "a.md"]);
  expect(harness.previewRequests).toHaveLength(5);
  // A freed retained slot is filled before the reusable preview; other surfaces are untouched.
  await act(async () => getSummaryProps().onCloseTab(getSummaryProps().tabs[1].id));
  await act(async () => {
    getChatProps().onOpenAgentSession("agent-1", "Agent");
    void getChatProps().onOpenWorkspacePath("f.md");
  });
  expect(
    getSummaryProps()
      .tabs.filter((tab) => tab.kind === "file")
      .map((tab) => tab.title),
  ).toEqual(["e.md", "c.md", "f.md", "a.md"]);
  expect(getSummaryProps().tabs.find((tab) => tab.title === "Agent")).toBeDefined();
  await act(async () => renderer.unmount());
});

test("workspace overview opens a reusable files content tab", async () => {
  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(<App />);
  });
  const overview = renderer.root.findByProps({ "aria-label": "Workspace overview" });
  await act(async () => overview.props.onClick());
  await act(async () =>
    renderer.root.findByProps({ "aria-label": "Browse files" }).props.onClick(),
  );
  expect(getSummaryProps().tabs).toMatchObject([{ kind: "files", title: "Files" }]);
  await act(async () => overview.props.onClick());
  await act(async () =>
    renderer.root.findByProps({ "aria-label": "Browse files" }).props.onClick(),
  );
  expect(getSummaryProps().tabs).toHaveLength(1);
  await act(async () => renderer.unmount());
});

test("opening the overview refreshes Git facts instead of keeping the initialization snapshot", async () => {
  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(<App />);
  });
  vi.mocked(getWorkspaceGitStatus).mockResolvedValueOnce({
    workspaceRoot: "D:\\Workspace",
    branch: "new-branch",
    changedFiles: [],
    totalAdded: 12,
    totalRemoved: 1,
    isGitRepository: true,
  });
  await act(async () =>
    renderer.root.findByProps({ "aria-label": "Workspace overview" }).props.onClick(),
  );
  expect(JSON.stringify(renderer.toJSON())).toContain("new-branch");
  await act(async () => renderer.unmount());
});

test("closing a loading file does not let its late response reopen the tab", async () => {
  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(<App />);
  });
  let operation!: Promise<void>;
  await act(async () => {
    operation = getChatProps().onOpenWorkspacePath("late.md");
  });
  const id = getSummaryProps().tabs[0].id;
  await act(async () => getSummaryProps().onCloseTab(id));
  await act(async () => {
    harness.previewRequests[0].resolve(preview("late.md", "late"));
    await operation;
  });
  expect(renderer.root.findByProps({ "aria-label": "Preview" }).props["aria-hidden"]).toBe(true);
  await act(async () => renderer.unmount());
});

test("reopening a loaded file focuses its existing view without resetting it to loading", async () => {
  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(<App />);
  });
  let operation!: Promise<void>;
  await act(async () => {
    operation = getChatProps().onOpenWorkspacePath("readme.md");
  });
  await act(async () => {
    harness.previewRequests[0].resolve(preview("readme.md", "readme"));
    await operation;
  });
  await act(async () => getSummaryProps().onCollapse());
  await act(async () => {
    void getChatProps().onOpenWorkspacePath("readme.md");
  });
  expect(harness.previewRequests).toHaveLength(1);
  expect(getSummaryProps().tabs).toMatchObject([{ content: "readme", loading: false }]);
  expect(renderer.root.findByProps({ "aria-label": "Preview" }).props["aria-hidden"]).toBe(false);
  await act(async () => renderer.unmount());
});

test("the latest request owns a file preview when reads finish out of order", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });

  let olderOperation: Promise<void> | null = null;
  let newerOperation: Promise<void> | null = null;
  await act(async () => {
    olderOperation = getChatProps().onOpenWorkspacePath("src/App.tsx", { startLine: 10 });
    newerOperation = getChatProps().onOpenWorkspacePath("src/App.tsx", { startLine: 20 });
  });
  expect(harness.previewRequests).toHaveLength(2);

  await act(async () => {
    harness.previewRequests[1].resolve(preview("src/App.tsx", "newer"));
    await newerOperation;
  });
  expect(getSummaryProps().tabs).toMatchObject([
    { content: "newer", targetLine: 20, loading: false },
  ]);

  await act(async () => {
    harness.previewRequests[0].resolve(preview("src/App.tsx", "older"));
    await olderOperation;
  });
  expect(getSummaryProps().tabs).toMatchObject([
    { content: "newer", targetLine: 20, loading: false },
  ]);

  await act(async () => renderer?.unmount());
});

test("Agent tabs are durable and closing the active tab selects its neighbor", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });

  await act(async () => {
    getChatProps().onOpenAgentSession("agent-1", "First title");
    getChatProps().onOpenAgentSession("agent-1", "Second title");
    getChatProps().onOpenAgentSession("agent-2", "Another Agent");
  });

  expect(getSummaryProps().tabs).toMatchObject([
    { id: "agent:agent-1", sessionId: "agent-1", title: "First title" },
    { id: "agent:agent-2", sessionId: "agent-2", title: "Another Agent" },
  ]);
  expect(getSummaryProps().activeTabId).toBe("agent:agent-2");
  expect(renderer!.root.findByProps({ "aria-label": "Preview" }).props["aria-hidden"]).toBe(false);

  await act(async () => getSummaryProps().onCloseTab("agent:agent-2"));
  expect(getSummaryProps().tabs).toHaveLength(1);
  expect(getSummaryProps().activeTabId).toBe("agent:agent-1");

  await act(async () => getSummaryProps().onCollapse());
  expect(renderer!.root.findByProps({ "aria-label": "Preview" }).props["aria-hidden"]).toBe(true);
  const showButton = renderer!.root.findByProps({ "aria-label": "Show right sidebar" });
  await act(async () => showButton.props.onClick());
  expect(renderer!.root.findByProps({ "aria-label": "Preview" }).props["aria-hidden"]).toBe(false);

  await act(async () => renderer?.unmount());
});
