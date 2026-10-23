import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, expect, test, vi } from "vitest";
import { useSessionController } from "../src/components/app/useSessionController";
import { useWorkspaceController } from "../src/components/app/useWorkspaceController";
import type { SessionItem } from "../src/lib/chatBridge";
import type { WorkspaceSnapshot } from "../src/lib/workspaceBridge";

vi.mock("../src/lib/chatBridge", () => ({ querySessionCatalog: vi.fn(), activateSession: vi.fn(async () => undefined), updateSession: vi.fn(), deleteSession: vi.fn(), reorderSessions: vi.fn() }));
vi.mock("../src/lib/workspaceBridge", () => ({ activateWorkspaceRoot: vi.fn(), getWorkspaceGitHubCliStatus: vi.fn(async () => ({ available: false, summary: "" })), getWorkspaceGitStatus: vi.fn(async () => ({ changedFiles: [] })), getWorkspaceInfo: vi.fn(), openWorkspaceFolder: vi.fn(), resetWorkspaceCatalog: vi.fn(), renameWorkspace: vi.fn(), removeWorkspace: vi.fn() }));
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const views: ReactTestRenderer[] = [];
afterEach(async () => {
  for (const view of views.splice(0)) await act(async () => view.unmount());
  vi.resetAllMocks();
});
async function mount() {
  let sessions!: ReturnType<typeof useSessionController>;
  let workspace!: ReturnType<typeof useWorkspaceController>;
  const report = vi.fn();
  function Harness() {
    sessions = useSessionController({ activeWorkspaceRoot: null, reportError: report });
    workspace = useWorkspaceController({ confirmAction: async () => false, reportHostError: report });
    return null;
  }
  await act(async () => { views.push(create(<Harness />)); });
  return { sessions: () => sessions, workspace: () => workspace };
}
const session = (id: string): SessionItem => ({ id, title: id, updatedAt: 1, sessionKind: "main", messageCount: 1, activityState: "idle" });
const snapshot = (root: string): WorkspaceSnapshot => ({ activeWorkspaceRoot: root, workspaces: [{ root, name: root, sortOrder: 0, updatedAt: 1 }], cancelled: false });

test("Desktop owners keep session selection, running flags and workspace catalogs isolated", async () => {
  const first = await mount();
  const second = await mount();
  await act(async () => {
    first.sessions().actions.beginInitialization().applySessions([session("one")], "one");
    first.sessions().actions.setRunning("one", true);
    first.workspace().actions.applySnapshot(snapshot("root-one"));
    second.sessions().actions.beginInitialization().applySessions([session("two")], "two");
    second.workspace().actions.applySnapshot(snapshot("root-two"));
  });
  expect(first.sessions().currentSessionId).toBe("one");
  expect([...first.sessions().runningSessionIds]).toEqual(["one"]);
  expect(second.sessions().currentSessionId).toBe("two");
  expect(second.sessions().runningSessionIds.size).toBe(0);
  expect(first.workspace().activeWorkspaceRoot).toBe("root-one");
  expect(second.workspace().activeWorkspaceRoot).toBe("root-two");
});

test("departed initialization cannot overwrite a new selection or workspace snapshot", async () => {
  const owner = await mount();
  const sessionInitialization = owner.sessions().actions.beginInitialization();
  const workspaceInitialization = owner.workspace().actions.beginInitialization();
  await act(async () => {
    owner.sessions().actions.resolveSession({ id: "new", title: "New", updatedAt: 1, messageCount: 1, sessionKind: "main" }, { activate: true });
    owner.workspace().actions.applySnapshot(snapshot("new-root"));
  });
  await act(async () => {
    expect(sessionInitialization.applySessions([session("old")], "old")).toBe(false);
    expect(workspaceInitialization.applySnapshot(snapshot("old-root"))).toBe(false);
  });
  expect(owner.sessions().currentSessionId).toBe("new");
  expect(owner.workspace().activeWorkspaceRoot).toBe("new-root");
});
