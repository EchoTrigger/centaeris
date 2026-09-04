import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { beforeEach, expect, test, vi } from "vitest";
import type { ResourceModalKind } from "../src/components/Sidebar";
import type { SummaryPanelTab } from "../src/components/SummaryPanel";
import type { AgentRuntimeConfig, SessionItem } from "../src/lib/chatBridge";
import type {
  WorkspaceGitStatusResponse,
  WorkspaceOpenMode,
  WorkspaceSnapshot,
} from "../src/lib/workspaceBridge";

type SidebarHarnessProps = {
  currentSessionId: string | null;
  activeWorkspaceRoot: string | null;
  workspaceCatalogError: { message: string; canReset: boolean } | null;
  onOpenWorkspace: (mode: WorkspaceOpenMode) => void;
  onSelectWorkspace: (root: string) => void;
  onSelectSession: (sessionId: string) => void;
  onRetryWorkspaceCatalog: () => Promise<void>;
  onResetWorkspaceCatalog: () => Promise<void>;
  onOpenResource: (kind: ResourceModalKind) => void;
};

type ChatHarnessProps = {
  currentSessionId: string | null;
  gitStatus: WorkspaceGitStatusResponse | null;
  runtimeConfigRevision: number;
  onOpenAgentSession: (sessionId: string, title: string) => void;
};

type SummaryHarnessProps = {
  tabs: SummaryPanelTab[];
};

type ConfirmHarnessProps = {
  open: boolean;
  title: string;
  onCancel: () => void;
  onConfirm: () => void;
};

type SkillsHarnessProps = {
  workspaceRoot?: string | null;
};

type GitStatusRequest = {
  root: string;
  promise: Promise<WorkspaceGitStatusResponse>;
  resolve: (status: WorkspaceGitStatusResponse) => void;
};

type WorkspaceRequest = {
  root: string;
  promise: Promise<WorkspaceSnapshot>;
  resolve: (snapshot: WorkspaceSnapshot) => void;
};

type RuntimeConfigRequest = {
  promise: Promise<AgentRuntimeConfig>;
  resolve: (config: AgentRuntimeConfig) => void;
};

const currentSession: SessionItem = {
  id: "current",
  title: "Current",
  updatedAt: 1,
  cwd: "D:\\Current",
  sessionKind: "main",
  messageCount: 1,
  activityState: "idle",
};

const externalSession: SessionItem = {
  id: "external",
  title: "External",
  updatedAt: 1,
  cwd: "D:\\External",
  sessionKind: "main",
  messageCount: 1,
  activityState: "idle",
};

const currentSnapshot: WorkspaceSnapshot = {
  activeWorkspaceRoot: "D:\\Current",
  workspaces: [{
    root: "D:\\Current",
    name: "Current",
    activeSessionId: "current",
    sortOrder: 0,
    updatedAt: 1,
  }],
  cancelled: false,
};

const runtimeConfig = (updatedAt: number, hasModel: boolean): AgentRuntimeConfig => ({
  executionHost: "localUser",
  autoContinueAfterResumeWait: false,
  modelProviders: [],
  selectableModels: hasModel ? [{
    providerId: "provider-one",
    providerName: "Provider One",
    model: `model-${updatedAt}`,
    modelThinkingModes: [],
  }] : [],
  updatedAt,
});

const harness = vi.hoisted(() => ({
  sidebarProps: null as SidebarHarnessProps | null,
  chatProps: null as ChatHarnessProps | null,
  summaryProps: null as SummaryHarnessProps | null,
  confirmProps: null as ConfirmHarnessProps | null,
  skillsProps: null as SkillsHarnessProps | null,
  runtimeListener: null as (() => void) | null,
  runtimeConfigCalls: 0,
  deferRuntimeConfigRefresh: false,
  runtimeConfigRequests: [] as RuntimeConfigRequest[],
  initialSnapshot: null as WorkspaceSnapshot | null,
  workspaceInfoResult: null as WorkspaceSnapshot | null,
  workspaceInfoError: null as unknown,
  openResult: null as WorkspaceSnapshot | null,
  selectResult: null as WorkspaceSnapshot | null,
  resetResult: null as WorkspaceSnapshot | null,
  resetCalls: 0,
  deferGitStatus: false,
  gitStatusRequests: [] as GitStatusRequest[],
  deferOpen: false,
  openRequests: [] as WorkspaceRequest[],
  deferSelect: false,
  selectRequests: [] as WorkspaceRequest[],
  deferRetry: false,
  deferInitialWorkspaceInfo: false,
  workspaceInfoCallCount: 0,
  initialWorkspaceInfoRequest: null as WorkspaceRequest | null,
  retryRequests: [] as WorkspaceRequest[],
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

vi.mock("../src/components/ConfirmDialog", () => ({
  ConfirmDialog: (props: ConfirmHarnessProps) => {
    harness.confirmProps = props;
    return null;
  },
}));

vi.mock("../src/components/SkillsDialog", () => ({
  SkillsDialog: (props: SkillsHarnessProps) => {
    harness.skillsProps = props;
    return <section data-testid="skills-dialog" />;
  },
}));

vi.mock("../src/components/ModelsDialog", () => ({ ModelsDialog: () => null }));
vi.mock("../src/components/PluginsDialog", () => ({ PluginsDialog: () => null }));

vi.mock("../src/lib/chatBridge", () => ({
  activateSession: vi.fn(async () => undefined),
  deleteSession: vi.fn(),
  getAgentRuntimeConfig: vi.fn(() => {
    harness.runtimeConfigCalls += 1;
    if (harness.runtimeConfigCalls > 1 && harness.deferRuntimeConfigRefresh) {
      let resolveConfig: (config: AgentRuntimeConfig) => void = () => {};
      const promise = new Promise<AgentRuntimeConfig>((resolve) => {
        resolveConfig = resolve;
      });
      harness.runtimeConfigRequests.push({ promise, resolve: resolveConfig });
      return promise;
    }
    return Promise.resolve(runtimeConfig(harness.runtimeConfigCalls, true));
  }),
  listenAgentRuntimeConfigChanges: vi.fn(async (listener: () => void) => {
    harness.runtimeListener = listener;
    return () => undefined;
  }),
  listSessions: vi.fn(async () => [currentSession, externalSession]),
  updateSession: vi.fn(),
}));

vi.mock("../src/lib/workspaceBridge", () => ({
  activateWorkspaceRoot: vi.fn((root: string) => {
    if (harness.deferSelect) {
      let resolveSelection: (snapshot: WorkspaceSnapshot) => void = () => {};
      const promise = new Promise<WorkspaceSnapshot>((resolve) => {
        resolveSelection = resolve;
      });
      harness.selectRequests.push({ root, promise, resolve: resolveSelection });
      return promise;
    }
    if (!harness.selectResult) throw new Error("missing select result");
    return Promise.resolve(harness.selectResult);
  }),
  getWorkspaceGitHubCliStatus: vi.fn(async () => ({ available: true, summary: "ready" })),
  getWorkspaceGitStatus: vi.fn((root: string) => {
    if (!harness.deferGitStatus) {
      return Promise.resolve({
        workspaceRoot: root,
        changedFiles: [],
        totalAdded: 0,
        totalRemoved: 0,
        isGitRepository: true,
      });
    }
    let resolveStatus: (status: WorkspaceGitStatusResponse) => void = () => {};
    const promise = new Promise<WorkspaceGitStatusResponse>((resolve) => {
      resolveStatus = resolve;
    });
    harness.gitStatusRequests.push({ root, promise, resolve: resolveStatus });
    return promise;
  }),
  getWorkspaceInfo: vi.fn(async () => {
    harness.workspaceInfoCallCount += 1;
    if (harness.workspaceInfoCallCount === 1 && harness.deferInitialWorkspaceInfo) {
      let resolveInitial: (snapshot: WorkspaceSnapshot) => void = () => {};
      const promise = new Promise<WorkspaceSnapshot>((resolve) => {
        resolveInitial = resolve;
      });
      harness.initialWorkspaceInfoRequest = {
        root: "initial",
        promise,
        resolve: resolveInitial,
      };
      return promise;
    }
    if (harness.workspaceInfoCallCount > 1 && harness.deferRetry) {
      let resolveRetry: (snapshot: WorkspaceSnapshot) => void = () => {};
      const promise = new Promise<WorkspaceSnapshot>((resolve) => {
        resolveRetry = resolve;
      });
      harness.retryRequests.push({ root: "retry", promise, resolve: resolveRetry });
      return promise;
    }
    if (harness.workspaceInfoError) throw harness.workspaceInfoError;
    const snapshot = harness.workspaceInfoResult ?? harness.initialSnapshot;
    if (!snapshot) throw new Error("missing workspace snapshot");
    harness.workspaceInfoResult = snapshot;
    return snapshot;
  }),
  openWorkspaceFolder: vi.fn((mode: WorkspaceOpenMode) => {
    if (harness.deferOpen) {
      let resolveOpen: (snapshot: WorkspaceSnapshot) => void = () => {};
      const promise = new Promise<WorkspaceSnapshot>((resolve) => {
        resolveOpen = resolve;
      });
      harness.openRequests.push({ root: mode, promise, resolve: resolveOpen });
      return promise;
    }
    if (!harness.openResult) throw new Error("missing open result");
    return Promise.resolve(harness.openResult);
  }),
  readDesktopFilePreview: vi.fn(),
  resetWorkspaceCatalog: vi.fn(async () => {
    harness.resetCalls += 1;
    if (!harness.resetResult) throw new Error("missing reset result");
    return { snapshot: harness.resetResult, quarantinedPath: "catalog.bak" };
  }),
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

const getConfirmProps = (): ConfirmHarnessProps => {
  if (!harness.confirmProps) throw new Error("ConfirmDialog has not rendered");
  return harness.confirmProps;
};

const gitStatus = (root: string): WorkspaceGitStatusResponse => ({
  workspaceRoot: root,
  changedFiles: [],
  totalAdded: 0,
  totalRemoved: 0,
  isGitRepository: true,
});

beforeEach(() => {
  harness.sidebarProps = null;
  harness.chatProps = null;
  harness.summaryProps = null;
  harness.confirmProps = null;
  harness.skillsProps = null;
  harness.runtimeListener = null;
  harness.runtimeConfigCalls = 0;
  harness.deferRuntimeConfigRefresh = false;
  harness.runtimeConfigRequests.length = 0;
  harness.initialSnapshot = currentSnapshot;
  harness.workspaceInfoResult = currentSnapshot;
  harness.workspaceInfoError = null;
  harness.openResult = null;
  harness.selectResult = null;
  harness.resetResult = null;
  harness.resetCalls = 0;
  harness.deferGitStatus = false;
  harness.gitStatusRequests.length = 0;
  harness.deferOpen = false;
  harness.openRequests.length = 0;
  harness.deferSelect = false;
  harness.selectRequests.length = 0;
  harness.deferRetry = false;
  harness.deferInitialWorkspaceInfo = false;
  harness.workspaceInfoCallCount = 0;
  harness.initialWorkspaceInfoRequest = null;
  harness.retryRequests.length = 0;
});

test("late bootstrap cannot overwrite a workspace opened after mount", async () => {
  harness.deferInitialWorkspaceInfo = true;
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
    await Promise.resolve();
  });
  expect(harness.initialWorkspaceInfoRequest).not.toBeNull();
  harness.openResult = {
    activeWorkspaceRoot: "D:\\New",
    workspaces: [{ root: "D:\\New", name: "New", sortOrder: 0, updatedAt: 2 }],
    cancelled: false,
  };

  await act(async () => {
    getSidebarProps().onOpenWorkspace("customPath");
    await Promise.resolve();
  });
  expect(getSidebarProps().activeWorkspaceRoot).toBe("D:\\New");
  expect(getSidebarProps().currentSessionId).toBeNull();

  const initialRequest = harness.initialWorkspaceInfoRequest;
  if (!initialRequest) throw new Error("Initial workspace request did not start");
  await act(async () => {
    initialRequest.resolve(currentSnapshot);
    await initialRequest.promise;
    await Promise.resolve();
  });
  expect(getSidebarProps().activeWorkspaceRoot).toBe("D:\\New");
  expect(getSidebarProps().currentSessionId).toBeNull();

  await act(async () => renderer?.unmount());
});

test("a cancelled folder picker preserves the current session and panel", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });
  await act(async () => getChatProps().onOpenAgentSession("child", "Child"));
  harness.openResult = {
    activeWorkspaceRoot: "D:\\Ignored",
    workspaces: [],
    cancelled: true,
  };

  await act(async () => {
    getSidebarProps().onOpenWorkspace("customPath");
    await Promise.resolve();
  });

  expect(getSidebarProps().activeWorkspaceRoot).toBe("D:\\Current");
  expect(getSidebarProps().currentSessionId).toBe("current");
  expect(getSummaryProps().tabs.map((tab) => tab.sessionId)).toEqual(["child"]);

  await act(async () => renderer?.unmount());
});

test("a successful folder open clears session and panel only after success", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });
  await act(async () => getChatProps().onOpenAgentSession("child", "Child"));
  harness.openResult = {
    activeWorkspaceRoot: "D:\\New",
    workspaces: [{ root: "D:\\New", name: "New", sortOrder: 0, updatedAt: 2 }],
    cancelled: false,
  };

  await act(async () => {
    getSidebarProps().onOpenWorkspace("customPath");
    await Promise.resolve();
  });

  expect(getSidebarProps().activeWorkspaceRoot).toBe("D:\\New");
  expect(getSidebarProps().currentSessionId).toBeNull();
  expect(renderer!.root.findAllByProps({ "data-testid": "summary-panel" })).toHaveLength(0);

  await act(async () => renderer?.unmount());
});

test("a late folder open cannot overwrite a newer workspace selection", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });
  harness.deferOpen = true;
  harness.deferSelect = true;

  await act(async () => {
    getSidebarProps().onOpenWorkspace("customPath");
    getSidebarProps().onSelectWorkspace("D:\\WorkspaceB");
  });
  expect(harness.openRequests).toHaveLength(1);
  expect(harness.selectRequests.map((request) => request.root)).toEqual(["D:\\WorkspaceB"]);

  await act(async () => {
    harness.selectRequests[0].resolve({
      activeWorkspaceRoot: "D:\\WorkspaceB",
      workspaces: [{ root: "D:\\WorkspaceB", name: "B", sortOrder: 0, updatedAt: 2 }],
      cancelled: false,
    });
    await harness.selectRequests[0].promise;
  });
  expect(getSidebarProps().activeWorkspaceRoot).toBe("D:\\WorkspaceB");

  await act(async () => {
    harness.openRequests[0].resolve({
      activeWorkspaceRoot: "D:\\WorkspaceA",
      workspaces: [{ root: "D:\\WorkspaceA", name: "A", sortOrder: 0, updatedAt: 1 }],
      cancelled: false,
    });
    await harness.openRequests[0].promise;
  });
  expect(getSidebarProps().activeWorkspaceRoot).toBe("D:\\WorkspaceB");

  await act(async () => renderer?.unmount());
});

test("an externally applied session workspace invalidates a pending catalog retry", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });
  harness.deferRetry = true;
  let retryOperation: Promise<void> | null = null;
  await act(async () => {
    retryOperation = getSidebarProps().onRetryWorkspaceCatalog();
  });
  expect(harness.retryRequests).toHaveLength(1);

  harness.selectResult = {
    activeWorkspaceRoot: "D:\\External",
    workspaces: [{ root: "D:\\External", name: "External", sortOrder: 0, updatedAt: 2 }],
    cancelled: false,
  };
  await act(async () => {
    getSidebarProps().onSelectSession("external");
    await Promise.resolve();
  });
  expect(getSidebarProps().activeWorkspaceRoot).toBe("D:\\External");

  await act(async () => {
    harness.retryRequests[0].resolve({
      activeWorkspaceRoot: "D:\\RetryOld",
      workspaces: [{ root: "D:\\RetryOld", name: "Old", sortOrder: 0, updatedAt: 1 }],
      cancelled: false,
    });
    await retryOperation;
  });
  expect(getSidebarProps().activeWorkspaceRoot).toBe("D:\\External");

  await act(async () => renderer?.unmount());
});

test("opening then cancelling reset confirmation does not steal workspace ownership", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });
  harness.deferOpen = true;
  let resetOperation: Promise<void> | null = null;
  await act(async () => {
    getSidebarProps().onOpenWorkspace("customPath");
    resetOperation = getSidebarProps().onResetWorkspaceCatalog();
    await Promise.resolve();
  });
  expect(getConfirmProps().open).toBe(true);

  await act(async () => {
    harness.openRequests[0].resolve({
      activeWorkspaceRoot: "D:\\WorkspaceA",
      workspaces: [{ root: "D:\\WorkspaceA", name: "A", sortOrder: 0, updatedAt: 2 }],
      cancelled: false,
    });
    await harness.openRequests[0].promise;
  });
  expect(getSidebarProps().activeWorkspaceRoot).toBe("D:\\WorkspaceA");

  await act(async () => {
    getConfirmProps().onCancel();
    await resetOperation;
  });
  expect(harness.resetCalls).toBe(0);
  expect(getSidebarProps().activeWorkspaceRoot).toBe("D:\\WorkspaceA");

  await act(async () => renderer?.unmount());
});

test("workspace reset has no side effects until it is confirmed", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });
  await act(async () => getChatProps().onOpenAgentSession("child", "Child"));
  let cancelledReset: Promise<void> | null = null;
  await act(async () => {
    cancelledReset = getSidebarProps().onResetWorkspaceCatalog();
    await Promise.resolve();
  });
  expect(getConfirmProps().open).toBe(true);
  await act(async () => {
    getConfirmProps().onCancel();
    await cancelledReset;
  });
  expect(harness.resetCalls).toBe(0);
  expect(getSidebarProps().currentSessionId).toBe("current");
  expect(getSummaryProps().tabs).toHaveLength(1);

  harness.resetResult = { activeWorkspaceRoot: null, workspaces: [], cancelled: false };
  let confirmedReset: Promise<void> | null = null;
  await act(async () => {
    confirmedReset = getSidebarProps().onResetWorkspaceCatalog();
    await Promise.resolve();
  });
  await act(async () => {
    getConfirmProps().onConfirm();
    await confirmedReset;
  });
  expect(harness.resetCalls).toBe(1);
  expect(getSidebarProps().activeWorkspaceRoot).toBeNull();
  expect(getSidebarProps().currentSessionId).toBeNull();

  await act(async () => renderer?.unmount());
});

test("workspace catalog failures stay distinct from ordinary host errors", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });
  harness.workspaceInfoError = new Error("workspace_catalog_corrupt: invalid JSON");
  await act(async () => getSidebarProps().onRetryWorkspaceCatalog());
  expect(getSidebarProps().workspaceCatalogError).toEqual({
    message: "workspace_catalog_corrupt: invalid JSON",
    canReset: true,
  });

  harness.workspaceInfoError = new Error("workspace directory is unavailable");
  await act(async () => getSidebarProps().onRetryWorkspaceCatalog());
  const hostAlert = renderer!.root.findByProps({ role: "alert" });
  expect(hostAlert.findByType("span").children.join(" ")).toBe(
    "workspace directory is unavailable",
  );

  await act(async () => renderer?.unmount());
});

test("an old Git status cannot overwrite the newly selected workspace", async () => {
  harness.deferGitStatus = true;
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });
  expect(harness.gitStatusRequests.map((request) => request.root)).toEqual(["D:\\Current"]);
  harness.selectResult = {
    activeWorkspaceRoot: "D:\\New",
    workspaces: [{ root: "D:\\New", name: "New", sortOrder: 0, updatedAt: 2 }],
    cancelled: false,
  };
  await act(async () => {
    getSidebarProps().onSelectWorkspace("D:\\New");
    await Promise.resolve();
  });
  expect(harness.gitStatusRequests.map((request) => request.root)).toEqual([
    "D:\\Current",
    "D:\\New",
  ]);

  await act(async () => {
    harness.gitStatusRequests[1].resolve(gitStatus("D:\\New"));
    await harness.gitStatusRequests[1].promise;
  });
  expect(getChatProps().gitStatus?.workspaceRoot).toBe("D:\\New");
  await act(async () => {
    harness.gitStatusRequests[0].resolve(gitStatus("D:\\Current"));
    await harness.gitStatusRequests[0].promise;
  });
  expect(getChatProps().gitStatus?.workspaceRoot).toBe("D:\\New");

  await act(async () => renderer?.unmount());
});

test("a runtime notification refreshes config and advances the ChatArea revision", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });
  expect(getChatProps().runtimeConfigRevision).toBe(0);
  expect(harness.runtimeConfigCalls).toBe(1);

  await act(async () => {
    harness.runtimeListener?.();
    await Promise.resolve();
  });
  expect(harness.runtimeConfigCalls).toBe(2);
  expect(getChatProps().runtimeConfigRevision).toBe(1);

  await act(async () => renderer?.unmount());
});

test("an older runtime config response cannot overwrite a newer notification", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });
  harness.deferRuntimeConfigRefresh = true;

  await act(async () => {
    harness.runtimeListener?.();
    harness.runtimeListener?.();
    await Promise.resolve();
  });
  expect(harness.runtimeConfigRequests).toHaveLength(2);

  await act(async () => {
    harness.runtimeConfigRequests[1].resolve(runtimeConfig(3, false));
    await harness.runtimeConfigRequests[1].promise;
    await Promise.resolve();
  });
  expect(renderer!.root.findAllByProps({ "data-testid": "chat-area" })).toHaveLength(0);

  await act(async () => {
    harness.runtimeConfigRequests[0].resolve(runtimeConfig(2, true));
    await harness.runtimeConfigRequests[0].promise;
    await Promise.resolve();
  });
  expect(renderer!.root.findAllByProps({ "data-testid": "chat-area" })).toHaveLength(0);

  await act(async () => renderer?.unmount());
});

test("Skills opens with an explicit null workspace when no project is active", async () => {
  harness.initialSnapshot = { activeWorkspaceRoot: null, workspaces: [], cancelled: false };
  harness.workspaceInfoResult = harness.initialSnapshot;
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<App />);
  });

  await act(async () => getSidebarProps().onOpenResource("skills"));
  expect(harness.skillsProps?.workspaceRoot).toBeNull();

  await act(async () => renderer?.unmount());
});
