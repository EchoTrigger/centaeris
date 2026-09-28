import { act, create } from "react-test-renderer";
import { expect, test } from "vitest";
import { useWorkspacePanelController } from "../src/components/app/useWorkspacePanelController";
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
test("task tabs stay attached to their session, deduplicate and close on session deletion", async () => {
  let controller!: ReturnType<typeof useWorkspacePanelController>;
  function Harness({ id }: { id: string }) {
    controller = useWorkspacePanelController({
      workspaceRoot: "root",
      sessions: [],
      currentSessionId: id,
    });
    return null;
  }
  let view!: ReturnType<typeof create>;
  await act(async () => {
    view = create(<Harness id="s1" />);
  });
  await act(async () => {
    controller.actions.openTasks();
    controller.actions.openTasks();
  });
  expect(controller.tabs).toMatchObject([{ kind: "tasks", sessionId: "s1", title: "Task" }]);
  await act(async () => view.update(<Harness id="s2" />));
  await act(async () => controller.actions.openTasks());
  expect(controller.tabs.map((t) => t.sessionId)).toEqual(["s1", "s2"]);
  await act(async () =>
    controller.actions.removeAgentSessions(new Set(["s1"])),
  );
  expect(controller.tabs.map((t) => t.sessionId)).toEqual(["s2"]);
  await act(async () => controller.actions.closeTab("tasks:s2"));
  expect(controller.tabs).toHaveLength(0);
  await act(async () => view.unmount());
});
