import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { beforeEach, expect, test, vi } from "vitest";
import type { DesktopFilePreviewReadResponse } from "../src/lib/workspaceBridge";
import type { SummaryPanelTab } from "../src/components/SummaryPanel";

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

vi.mock("../src/lib/chatBridge", () => ({
  activateSession: vi.fn(async () => undefined),
  deleteSession: vi.fn(),
  getAgentRuntimeConfig: vi.fn(async () => ({ selectableModels: [{ model: "test" }] })),
  listenAgentRuntimeConfigChanges: vi.fn(async () => () => undefined),
  listSessions: vi.fn(async () => []),
  updateSession: vi.fn(),
}));

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
    workspaces: [{
      root: "D:\\Workspace",
      name: "Workspace",
      sortOrder: 0,
      updatedAt: 1,
    }],
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
  harness.chatProps = null;
  harness.summaryProps = null;
  harness.previewRequests.length = 0;
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
