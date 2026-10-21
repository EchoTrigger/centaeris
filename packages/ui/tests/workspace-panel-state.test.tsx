import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, expect, test, vi } from "vitest";
import { useWorkspacePanelController } from "../src/components/app/useWorkspacePanelController";
import { readDesktopFilePreview } from "../src/lib/workspaceBridge";

vi.mock("../src/lib/workspaceBridge", () => ({ readDesktopFilePreview: vi.fn() }));
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const views: ReactTestRenderer[] = [];
afterEach(async () => {
  for (const view of views.splice(0)) await act(async () => view.unmount());
  vi.resetAllMocks();
});
type Panel = ReturnType<typeof useWorkspacePanelController>;
async function mount(root = "root") {
  let panel!: Panel;
  function Harness() {
    panel = useWorkspacePanelController({ workspaceRoot: root, sessions: [{ id: "main", title: "Main", updatedAt: 1, messageCount: 1, sessionKind: "main" }], currentSessionId: "main" });
    return null;
  }
  await act(async () => { views.push(create(<Harness />)); });
  return () => panel;
}
function pendingPreview() {
  let finish!: (result: Awaited<ReturnType<typeof readDesktopFilePreview>>) => void;
  vi.mocked(readDesktopFilePreview).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  return (path: string, content: string) => finish({ root: "root", path, name: path, content, byteLen: content.length, encoding: "utf-8", contentKind: "text", mimeType: "text/plain" });
}

test("workspace scopes restore their tabs, selection and collapsed state while invalidating departed reads", async () => {
  const panel = await mount();
  const finish = pendingPreview();
  let read!: Promise<void>;
  await act(async () => { read = panel().actions.openFilePath("pending.md"); });
  await act(async () => panel().actions.collapse());
  await act(async () => {
    panel().actions.saveScope("session:a");
    panel().actions.restoreScope("session:b");
  });
  expect(panel().tabs).toEqual([]);
  expect(panel().isOpen).toBe(true);
  await act(async () => panel().actions.openAgentSession("agent-b", "Agent B"));
  await act(async () => {
    panel().actions.saveScope("session:b");
    panel().actions.restoreScope("session:a");
  });
  expect(panel().tabs).toMatchObject([{ path: "pending.md", loading: false, error: "Preview interrupted. Reopen this file to retry." }]);
  expect(panel().activeTabId).toBe(panel().tabs[0].id);
  expect(panel().isOpen).toBe(false);
  await act(async () => { finish("pending.md", "departed response"); await read; });
  expect(panel().tabs[0].content).toBeUndefined();
  await act(async () => panel().actions.restoreScope("session:b"));
  expect(panel().tabs.map(tab => tab.sessionId)).toEqual(["agent-b"]);
});

test("independent Desktop panel owners do not share tabs or saved scopes", async () => {
  const first = await mount("root-a");
  const second = await mount("root-b");
  await act(async () => {
    first().actions.openAgentSession("first", "First");
    first().actions.saveScope("same-key");
  });
  await act(async () => second().actions.restoreScope("same-key"));
  expect(second().tabs).toEqual([]);
  expect(first().tabs.map(tab => tab.sessionId)).toEqual(["first"]);
});

test("closing the selected tab chooses the adjacent tab and removing parent sessions removes their surfaces", async () => {
  const panel = await mount();
  await act(async () => {
    panel().actions.openAgentSession("one", "One");
    panel().actions.openAgentSession("two", "Two");
    panel().actions.openAgentSession("three", "Three");
  });
  await act(async () => panel().actions.selectTab("agent:two"));
  await act(async () => panel().actions.closeTab("agent:two"));
  expect(panel().activeTabId).toBe("agent:three");
  await act(async () => panel().actions.closeTab("agent:three"));
  expect(panel().activeTabId).toBe("agent:one");
  await act(async () => panel().actions.removeAgentSessions(new Set(["main"])));
  expect(panel().tabs).toEqual([]);
  expect(panel().activeTabId).toBeNull();
});
