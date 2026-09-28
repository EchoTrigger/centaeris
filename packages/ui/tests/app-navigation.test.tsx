import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, test, vi } from "vitest";
import { useAppNavigation, type AppLocation } from "../src/components/app/useAppNavigation";
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
test("back restores session and workspace, failed navigation preserves forward history", async () => {
  let nav!: ReturnType<typeof useAppNavigation>;
  let current: AppLocation = { page: "chat", sessionId: "a", root: "A" };
  let view!: ReactTestRenderer;
  const apply = vi.fn(async (next: AppLocation) => { current = next; view.update(<Harness />); return true; });
  function Harness() { nav = useAppNavigation(current, apply); return null; }
  await act(async () => { view = create(<Harness />); });
  await act(async () => nav.move({ page: "plugins", sessionId: "a", root: "A" }));
  await act(async () => nav.move({ page: "chat", sessionId: "b", root: "B" }));
  await act(async () => nav.move(-1)); expect(current.page).toBe("plugins");
  await act(async () => nav.move(-1)); expect(current).toEqual({ page: "chat", sessionId: "a", root: "A" });
  expect(nav.canForward).toBe(true);
  apply.mockResolvedValueOnce(false);
  await act(async () => nav.move({ page: "chat", sessionId: "missing", root: "missing" }));
  expect(nav.canForward).toBe(true);
  await act(async () => nav.move({ page: "settings", sessionId: "a", root: "A" }));
  expect(nav.canForward).toBe(false);
  await act(async () => view.unmount());
});
