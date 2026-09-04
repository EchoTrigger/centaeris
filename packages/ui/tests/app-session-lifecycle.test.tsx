import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { beforeEach, expect, test, vi } from "vitest";
import type { SessionItem } from "../src/lib/chatBridge";
import type { UiSession } from "../src/types/ui";
import type { WorkspaceSnapshot } from "../src/lib/workspaceBridge";

type WorkspaceActivation = {
  root: string;
  promise: Promise<WorkspaceSnapshot>;
  resolve: (snapshot: WorkspaceSnapshot) => void;
};

type SessionRefreshRequest = {
  promise: Promise<SessionItem[]>;
  resolve: (sessions: SessionItem[]) => void;
};

type SidebarHarnessProps = {
  sessions: UiSession[];
  currentSessionId: string | null;
  runningSessionIds: Set<string>;
  completedSessionIds: Set<string>;
  onNewChat: () => void;
  onSelectSession: (sessionId: string) => void;
  onDeleteSession: (sessionId: string) => Promise<void>;
};

type ChatHarnessProps = {
  currentSessionId: string | null;
  onAgentRunningChange: (sessionId: string, running: boolean) => void;
  onOpenAgentSession: (sessionId: string, title: string) => void;
  onSessionResolved: (session: UiSession, options?: { activate?: boolean }) => void;
  onSessionCompleted: (sessionId: string) => void;
};

type SummaryHarnessProps = {
  tabs: Array<{ id: string; sessionId?: string }>;
};

const makeSession = (
  id: string,
  cwd: string,
  sessionKind: SessionItem["sessionKind"] = "main",
  parentSessionId?: string,
): SessionItem => ({
  id,
  title: `Session ${id}`,
  updatedAt: 1,
  cwd,
  sessionKind,
  parentSessionId,
  messageCount: 1,
  activityState: "idle",
});

const initialSessions = [
  makeSession("current", "D:\\Current"),
  makeSession("session-local", "D:\\Current"),
  makeSession("session-a", "D:\\WorkspaceA"),
  makeSession("session-b", "D:\\WorkspaceB"),
  makeSession("child-a", "D:\\WorkspaceA", "subagent", "session-a"),
  makeSession("child-b", "D:\\WorkspaceB", "subagent", "session-b"),
];

const harness = vi.hoisted(() => ({
  sidebarProps: null as SidebarHarnessProps | null,
  chatProps: null as ChatHarnessProps | null,
  summaryProps: null as SummaryHarnessProps | null,
  activations: [] as WorkspaceActivation[],
  listedSessions: [] as SessionItem[],
  deleteResultId: "",
  listSessionsCallCount: 0,
  deferSessionRefresh: false,
  sessionRefreshRequests: [] as SessionRefreshRequest[],
}));

vi.mock("../src/components/Sidebar", () => ({
  Sidebar: (props: SidebarHarnessProps) => {
    harness.sidebarProps = props;
    return <aside data-testid="sidebar" />;
  },
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

vi.mock("../src/components/ModelsDialog", () => ({ ModelsDialog: () => null }));
vi.mock("../src/components/SkillsDialog", () => ({ SkillsDialog: () => null }));
vi.mock("../src/components/PluginsDialog", () => ({ PluginsDialog: () => null }));
vi.mock("../src/components/ConfirmDialog", () => ({ ConfirmDialog: () => null }));

vi.mock("../src/lib/chatBridge", () => ({
  activateSession: vi.fn(async () => undefined),
  deleteSession: vi.fn(async () => ({ deletedSessionId: harness.deleteResultId })),
  getAgentRuntimeConfig: vi.fn(async () => ({ selectableModels: [{ model: "test" }] })),
  listenAgentRuntimeConfigChanges: vi.fn(async () => () => undefined),
  listSessions: vi.fn(() => {
    harness.listSessionsCallCount += 1;
    if (harness.listSessionsCallCount === 1 || !harness.deferSessionRefresh) {
      return Promise.resolve(harness.listedSessions);
    }
    let resolveRefresh: (sessions: SessionItem[]) => void = () => {};
    const promise = new Promise<SessionItem[]>((resolve) => {
      resolveRefresh = resolve;
    });
    harness.sessionRefreshRequests.push({ promise, resolve: resolveRefresh });
    return promise;
  }),
  updateSession: vi.fn(async (sessionId: string) => {
    const session = initialSessions.find((item) => item.id === sessionId);
    if (!session) throw new Error(`missing session ${sessionId}`);
    return session;
  }),
}));

vi.mock("../src/lib/workspaceBridge", () => ({
  activateWorkspaceRoot: vi.fn((root: string) => {
    let resolveActivation: (snapshot: WorkspaceSnapshot) => void = () => {};
    const promise = new Promise<WorkspaceSnapshot>((resolve) => {
      resolveActivation = resolve;
    });
    harness.activations.push({ root, promise, resolve: resolveActivation });
    return promise;
  }),
  getWorkspaceGitHubCliStatus: vi.fn(async () => ({ available: false, summary: "" })),
  getWorkspaceGitStatus: vi.fn(async (workspaceRoot: string) => ({
    workspaceRoot,
    changedFiles: [],
    totalAdded: 0,
    totalRemoved: 0,
    isGitRepository: true,
  })),
  getWorkspaceInfo: vi.fn(async () => ({
    activeWorkspaceRoot: "D:\\Current",
    workspaces: [
      { root: "D:\\Current", name: "Current", activeSessionId: "current", sortOrder: 0, updatedAt: 1 },
      { root: "D:\\WorkspaceA", name: "A", sortOrder: 1, updatedAt: 1 },
      { root: "D:\\WorkspaceB", name: "B", sortOrder: 2, updatedAt: 1 },
    ],
    cancelled: false,
  })),
  openWorkspaceFolder: vi.fn(),
  readDesktopFilePreview: vi.fn(),
  resetWorkspaceCatalog: vi.fn(),
}));

import App from "../src/App";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const getSidebarProps = (): SidebarHarnessProps => {
  if (!harness.sidebarProps) throw new Error("Sidebar has not rendered");
  return harness.sidebarProps;
};

const getChatProps = (): ChatHarnessProps => {
  if (!harness.chatProps) throw new Error("ChatArea has not rendered");
  return harness.chatProps;
};

const getSummaryProps = (): SummaryHarnessProps => {
  if (!harness.summaryProps) throw new Error("SummaryPanel has not rendered");
  return harness.summaryProps;
};

beforeEach(() => {
  harness.sidebarProps = null;
  harness.chatProps = null;
  harness.summaryProps = null;
  harness.activations.length = 0;
  harness.listedSessions = initialSessions;
  harness.deleteResultId = "";
  harness.listSessionsCallCount = 0;
  harness.deferSessionRefresh = false;
  harness.sessionRefreshRequests.length = 0;
});

test("the latest selection owns cross-workspace activation", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });
  expect(getSidebarProps().currentSessionId).toBe("current");

  await act(async () => {
    getSidebarProps().onSelectSession("session-a");
    getSidebarProps().onSelectSession("session-b");
  });
  expect(harness.activations.map((request) => request.root)).toEqual([
    "D:\\WorkspaceA",
    "D:\\WorkspaceB",
  ]);

  await act(async () => {
    harness.activations[1].resolve({
      activeWorkspaceRoot: "D:\\WorkspaceB",
      workspaces: [],
      cancelled: false,
    });
    await harness.activations[1].promise;
  });
  expect(getSidebarProps().currentSessionId).toBe("session-b");

  await act(async () => {
    harness.activations[0].resolve({
      activeWorkspaceRoot: "D:\\WorkspaceA",
      workspaces: [],
      cancelled: false,
    });
    await harness.activations[0].promise;
  });
  expect(getSidebarProps().currentSessionId).toBe("session-b");

  await act(async () => renderer?.unmount());
});

test("a subagent session cannot replace the selected main session", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });

  await act(async () => getSidebarProps().onSelectSession("child-a"));
  expect(getSidebarProps().currentSessionId).toBe("current");
  expect(harness.activations).toHaveLength(0);

  await act(async () => renderer?.unmount());
});

test("a mismatched delete response cannot clean local session state", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });
  await act(async () => {
    getChatProps().onOpenAgentSession("child-a", "Child A");
    getChatProps().onAgentRunningChange("child-a", true);
  });
  harness.deleteResultId = "different-session";

  await act(async () => {
    await expect(getSidebarProps().onDeleteSession("session-a")).rejects.toThrow(
      "删除会话响应身份不匹配",
    );
  });

  expect(getSidebarProps().sessions.some((session) => session.id === "session-a")).toBe(true);
  expect(getSidebarProps().runningSessionIds.has("child-a")).toBe(true);
  expect(getSummaryProps().tabs.some((tab) => tab.sessionId === "child-a")).toBe(true);

  await act(async () => renderer?.unmount());
});

test("deleting a main session cleans child activity and Agent tabs", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });
  await act(async () => {
    getChatProps().onOpenAgentSession("child-a", "Child A");
    getChatProps().onOpenAgentSession("child-b", "Child B");
    getChatProps().onSessionCompleted("child-a");
    getChatProps().onAgentRunningChange("child-a", true);
  });
  expect(getSidebarProps().runningSessionIds.has("child-a")).toBe(true);
  expect(getSidebarProps().completedSessionIds.has("child-a")).toBe(true);
  harness.deleteResultId = "session-a";
  harness.listedSessions = initialSessions.filter(
    (session) => session.id !== "session-a" && session.parentSessionId !== "session-a",
  );

  await act(async () => {
    await getSidebarProps().onDeleteSession("session-a");
  });

  expect(getSidebarProps().runningSessionIds.has("child-a")).toBe(false);
  expect(getSidebarProps().completedSessionIds.has("child-a")).toBe(false);
  expect(getSummaryProps().tabs.map((tab) => tab.sessionId)).toEqual(["child-b"]);

  await act(async () => renderer?.unmount());
});

test("completion distinguishes the current session from a background main session", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });
  harness.deferSessionRefresh = true;

  await act(async () => {
    getChatProps().onAgentRunningChange("current", true);
    getChatProps().onSessionCompleted("current");
  });
  expect(getSidebarProps().runningSessionIds.has("current")).toBe(false);
  expect(getSidebarProps().completedSessionIds.has("current")).toBe(false);
  expect(getSidebarProps().sessions.find((session) => session.id === "current")?.isUnread).toBe(false);

  await act(async () => {
    getChatProps().onAgentRunningChange("session-a", true);
    getChatProps().onSessionCompleted("session-a");
  });
  expect(getSidebarProps().runningSessionIds.has("session-a")).toBe(false);
  expect(getSidebarProps().completedSessionIds.has("session-a")).toBe(true);
  expect(getSidebarProps().sessions.find((session) => session.id === "session-a")?.isUnread).toBe(true);

  await act(async () => renderer?.unmount());
});

test("a completion refresh updates the list without overriding a newer manual selection", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });
  harness.deferSessionRefresh = true;

  await act(async () => {
    getChatProps().onSessionCompleted("session-a");
    await Promise.resolve();
  });
  expect(harness.sessionRefreshRequests).toHaveLength(1);
  await act(async () => {
    getSidebarProps().onSelectSession("session-local");
    await Promise.resolve();
  });
  expect(getSidebarProps().currentSessionId).toBe("session-local");

  const refreshedSessions = initialSessions.map((session) =>
    session.id === "current" ? { ...session, title: "Current refreshed" } : session
  );
  await act(async () => {
    harness.sessionRefreshRequests[0].resolve(refreshedSessions);
    await harness.sessionRefreshRequests[0].promise;
    await Promise.resolve();
  });
  expect(getSidebarProps().sessions.find((session) => session.id === "current")?.title)
    .toBe("Current refreshed");
  expect(getSidebarProps().currentSessionId).toBe("session-local");

  await act(async () => renderer?.unmount());
});

test("a delete refresh updates the list without overriding a newer manual selection", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });
  harness.deferSessionRefresh = true;
  harness.deleteResultId = "session-a";
  let deleteOperation: Promise<void> | null = null;
  await act(async () => {
    deleteOperation = getSidebarProps().onDeleteSession("session-a");
    await Promise.resolve();
  });
  expect(harness.sessionRefreshRequests).toHaveLength(1);
  await act(async () => {
    getSidebarProps().onSelectSession("session-local");
    await Promise.resolve();
  });
  expect(getSidebarProps().currentSessionId).toBe("session-local");

  const refreshedSessions = initialSessions.filter(
    (session) => session.id !== "session-a" && session.parentSessionId !== "session-a",
  );
  await act(async () => {
    harness.sessionRefreshRequests[0].resolve(refreshedSessions);
    await deleteOperation;
  });
  expect(getSidebarProps().sessions.some((session) => session.id === "session-a")).toBe(false);
  expect(getSidebarProps().currentSessionId).toBe("session-local");

  await act(async () => renderer?.unmount());
});

test("an activated resolved session supersedes a pending workspace selection", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });
  await act(async () => getSidebarProps().onSelectSession("session-a"));
  const resolvedSession: UiSession = {
    id: "resolved",
    title: "Resolved",
    messageCount: 0,
    sessionKind: "main",
    cwd: "D:\\Current",
  };
  await act(async () => {
    getChatProps().onSessionResolved(resolvedSession, { activate: true });
  });
  expect(getSidebarProps().currentSessionId).toBe("resolved");

  await act(async () => {
    harness.activations[0].resolve({
      activeWorkspaceRoot: "D:\\WorkspaceA",
      workspaces: [],
      cancelled: false,
    });
    await harness.activations[0].promise;
  });
  expect(getSidebarProps().currentSessionId).toBe("resolved");

  await act(async () => renderer?.unmount());
});

test("clearing selection supersedes a pending workspace selection", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });
  await act(async () => {
    getSidebarProps().onSelectSession("session-a");
    getSidebarProps().onNewChat();
  });
  expect(getSidebarProps().currentSessionId).toBeNull();

  await act(async () => {
    harness.activations[0].resolve({
      activeWorkspaceRoot: "D:\\WorkspaceA",
      workspaces: [],
      cancelled: false,
    });
    await harness.activations[0].promise;
  });
  expect(getSidebarProps().currentSessionId).toBeNull();

  await act(async () => renderer?.unmount());
});
