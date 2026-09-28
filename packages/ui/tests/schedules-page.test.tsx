import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, test, vi } from "vitest";
import { SchedulesPage } from "../src/components/SchedulesPage";
import { invokeHost } from "../src/host/hostBridge";
vi.mock("../src/host/hostBridge", () => ({ invokeHost: vi.fn() }));
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
test("schedules show the host list and open the selected occurrence chat", async () => {
  vi.mocked(invokeHost).mockImplementation(async (_command, payload) => (payload?.request as { action?: string })?.action === "list" ? { serviceEnabled: true, schedules: [{ id: "p", enabled: true, nextAt: null, spec: { name: "Daily check", cwd: "root", prompt: "Review files", timezone: "UTC", cron: "0 9 * * *" } }] } : { runs: [{ id: "r", scheduledAt: 1, status: "completed", sessionId: "s", error: null }] });
  let view!: ReactTestRenderer; const open = vi.fn();
  await act(async () => { view = create(<SchedulesPage onSession={open} />); });
  await act(async () => view.root.findByProps({ "aria-pressed": false }).props.onClick());
  expect(invokeHost).toHaveBeenCalledWith("schedule_manage", { request: { action: "history", scheduleId: "p" } });
  await act(async () => view.root.findAllByType("button").find(b => b.children.includes("Open chat"))!.props.onClick());
  expect(open).toHaveBeenCalledWith("s");
  expect(vi.mocked(invokeHost).mock.calls.every(([, payload]) => ["list", "history"].includes(String((payload?.request as { action?: string })?.action)))).toBe(true);
  await act(async () => view.unmount());
});
