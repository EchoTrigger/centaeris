import { act, create } from "react-test-renderer";
import { expect, test, vi } from "vitest";
import { useSessionController } from "../src/components/app/useSessionController";
import { reorderSessions, listSessions, type SessionItem } from "../src/lib/chatBridge";
vi.mock("../src/lib/chatBridge", async () => {
 const { catalogFixture } = await import("./catalogFixture");
 const bridge = { reorderSessions: vi.fn(), listSessions: vi.fn() };
 return {...bridge, querySessionCatalog: catalogFixture(bridge.listSessions)};
});
vi.mock("../src/lib/workspaceBridge", () => ({ activateWorkspaceRoot: vi.fn() }));
const item = (id: string, pinned: boolean, order: number, updatedAt = 1): SessionItem => ({ id, title: id, cwd: "D:/A", isPinned: pinned, sortOrder: order, updatedAt, messageCount: 1, activityState: "idle", sessionKind: "main" });

test("pinned order is optimistic and persisted without replacing unpinned chats", async () => {
  let controller!: ReturnType<typeof useSessionController>;
  const reportError = vi.fn();
  function Harness() { controller = useSessionController({ activeWorkspaceRoot: null, reportError }); return null; }
  let view!: ReturnType<typeof create>;
  await act(async () => { view = create(<Harness />); });
  await act(async () => { controller.actions.beginInitialization().applySessions([item("a", true, 0), item("b", true, 1), item("old", false, 0), item("new", false, 9, 3)]); });
  let finish!: (items: SessionItem[]) => void;
  vi.mocked(reorderSessions).mockImplementation(() => new Promise(resolve => { finish = resolve; }));
  let pending!: Promise<void>;
  await act(async () => { pending = controller.actions.reorderPinnedSessions(["b", "a"]); });
  expect(controller.mainSessions.map(session => session.id)).toEqual(["b", "a", "new", "old"]);
  expect(reorderSessions).toHaveBeenCalledWith("pinned", ["b", "a"]);
  await act(async () => { finish([item("b", true, 0), item("a", true, 1)]); await pending; });
  expect(controller.mainSessions.map(session => session.id)).toEqual(["b", "a", "new", "old"]);
  // A timed-out write may already have committed: read once instead of resending.
  vi.mocked(reorderSessions).mockRejectedValueOnce(new Error("timeout"));
  vi.mocked(listSessions).mockResolvedValueOnce([item("b", true, 0), item("a", true, 1), item("old", false, 0), item("new", false, 9, 3)]);
  await act(async () => { await expect(controller.actions.reorderPinnedSessions(["a", "b"])).rejects.toThrow("timeout"); });
  expect(controller.mainSessions.map(session => session.id)).toEqual(["b", "a", "new", "old"]);
  expect(reorderSessions).toHaveBeenCalledTimes(2);
  expect(reportError).toHaveBeenCalledOnce();
  await act(async () => view.unmount());
});
