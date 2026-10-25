// @vitest-environment jsdom
import { act } from "react";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { SchedulesPage } from "../src/components/SchedulesPage";
import { invokeHost } from "../src/host/hostBridge";
vi.mock("../src/host/hostBridge", () => ({ invokeHost: vi.fn() }));
const plan = (id: string, enabled = true) => ({ id, enabled, nextAt: null, spec: { name: id, prompt: `Review ${id}`, cwd: "/project", timezone: "UTC", cron: "0 9 * * *", at: null } });
const run = (id: string) => ({ id, scheduledAt: 1, status: "completed", sessionId: `chat-${id}`, error: null });
beforeEach(() => { Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true }); });
beforeEach(() => vi.mocked(invokeHost).mockImplementation(async (_command, payload) => (payload?.request as { action: string }).action === "list" ? { serviceEnabled: true, schedules: [plan("Daily check")], nextCursor: null } : { runs: [run("one")], nextCursor: null }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

test("schedules show the host list, open occurrence chats and explain agent-based creation", async () => {
  const user = userEvent.setup(), open = vi.fn(), create = vi.fn();
  render(<SchedulesPage onSession={open} onCreate={create} />);
  await user.click(await screen.findByRole("button", { name: "Open schedule Daily check" }));
  expect(invokeHost).toHaveBeenCalledWith("schedule_manage", { request: { action: "history", scheduleId: "Daily check" } });
  await user.click(await screen.findByRole("button", { name: "Open chat" }));
  expect(open).toHaveBeenCalledWith("chat-one");
  await user.click(screen.getByRole("button", { name: "Back to schedules" }));
  await user.click(screen.getByRole("button", { name: "Create with agent" }));
  expect(create).toHaveBeenCalledOnce();
  expect(vi.mocked(invokeHost).mock.calls.every(([, payload]) => ["list", "history"].includes(String((payload?.request as { action: string }).action)))).toBe(true);
});

test("schedule pagination preserves previous rows and retries the failed page cursor", async () => {
  let failed = false;
  vi.mocked(invokeHost).mockImplementation(async (_command, payload) => {
    const request = payload?.request as { cursor?: string };
    if (!request.cursor) return { serviceEnabled: true, schedules: [plan("first")], nextCursor: "next" };
    if (!failed) { failed = true; throw new Error("Page unavailable"); }
    return { serviceEnabled: true, schedules: [plan("second")], nextCursor: null };
  });
  const user = userEvent.setup(); render(<SchedulesPage onSession={() => {}} />);
  await user.click(await screen.findByRole("button", { name: "Load more schedules" }));
  expect(await screen.findByRole("alert")).toHaveProperty("textContent", expect.stringContaining("Page unavailable"));
  expect(screen.getByRole("button", { name: "Open schedule first" })).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Retry" }));
  await screen.findByRole("button", { name: "Open schedule second" });
  expect(screen.getByRole("button", { name: "Open schedule first" })).toBeTruthy();
  expect(invokeHost).toHaveBeenLastCalledWith("schedule_manage", { request: { action: "list", cursor: "next" } });
});

test("loading history does not claim there are no runs and departed responses cannot replace another plan", async () => {
  let finish!: (value: { runs: ReturnType<typeof run>[]; nextCursor: null }) => void;
  vi.mocked(invokeHost).mockImplementation(async (_command, payload) => {
    const request = payload?.request as { action: string; scheduleId?: string };
    if (request.action === "list") return { serviceEnabled: true, schedules: [plan("first"), plan("second")], nextCursor: null };
    if (request.scheduleId === "first") return new Promise(resolve => { finish = resolve; });
    return { runs: [run("second")], nextCursor: null };
  });
  const user = userEvent.setup(), open = vi.fn(); render(<SchedulesPage onSession={open} />);
  await user.click(await screen.findByRole("button", { name: "Open schedule first" }));
  expect(screen.getByRole("status")).toHaveProperty("textContent", expect.stringContaining("Loading run history"));
  expect(screen.queryByText("No runs yet")).toBeNull();
  await user.click(screen.getByRole("button", { name: "Back to schedules" }));
  await user.click(screen.getByRole("button", { name: "Open schedule second" }));
  await screen.findByRole("button", { name: "Open chat" });
  await act(async () => finish({ runs: [run("old")], nextCursor: null }));
  // The selected heading and current plan's history remain owned by the second plan.
  expect(screen.getByRole("heading", { name: "second" })).toBeTruthy();
  expect(screen.getAllByRole("button", { name: "Open chat" })).toHaveLength(1);
  await user.click(screen.getByRole("button", { name: "Open chat" }));
  expect(open).toHaveBeenCalledWith("chat-second");
});

test("run pagination retains earlier runs and failure recovery keeps its selected plan", async () => {
  let fail = true;
  vi.mocked(invokeHost).mockImplementation(async (_command, payload) => {
    const request = payload?.request as { action: string; cursor?: string };
    if (request.action === "list") return { serviceEnabled: true, schedules: [plan("Daily check")], nextCursor: null };
    if (!request.cursor) return { runs: [run("first")], nextCursor: "older" };
    if (fail) { fail = false; throw new Error("History unavailable"); }
    return { runs: [run("second")], nextCursor: null };
  });
  const user = userEvent.setup(), open = vi.fn(); render(<SchedulesPage onSession={open} />);
  await user.click(await screen.findByRole("button", { name: "Open schedule Daily check" }));
  await user.click(await screen.findByRole("button", { name: "Load more runs" }));
  await screen.findByRole("alert");
  await user.click(screen.getByRole("button", { name: "Retry" }));
  await waitFor(() => expect(screen.getAllByRole("button", { name: "Open chat" })).toHaveLength(2));
  await user.click(screen.getAllByRole("button", { name: "Open chat" })[0]);
  expect(open).toHaveBeenCalledWith("chat-second");
  expect(invokeHost).toHaveBeenLastCalledWith("schedule_manage", { request: { action: "history", scheduleId: "Daily check", cursor: "older" } });
});

test("search and paused-state filters remain intact after visiting a schedule", async () => {
  vi.mocked(invokeHost).mockImplementation(async (_command, payload) => (payload?.request as { action: string }).action === "list" ? { serviceEnabled: false, schedules: [plan("Active"), plan("Paused", false)], nextCursor: null } : { runs: [], nextCursor: null });
  const user = userEvent.setup(); render(<SchedulesPage onSession={() => {}} />);
  await screen.findByRole("button", { name: "Open schedule Active" });
  expect(screen.getByText("Scheduling paused")).toBeTruthy();
  await user.click(screen.getByRole("combobox", { name: "Schedule status" }));
  await user.click(await screen.findByRole("option", { name: "Paused" }));
  expect(screen.queryByRole("button", { name: "Open schedule Active" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Open schedule Paused" }));
  await screen.findByText("No runs yet");
  await user.click(screen.getByRole("button", { name: "Back to schedules" }));
  expect(screen.queryByRole("button", { name: "Open schedule Active" })).toBeNull();
  await user.type(screen.getByRole("textbox", { name: "Search schedules" }), "missing");
  expect(screen.getByText("No matching schedules")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Clear filters" }));
  expect(screen.getByRole("button", { name: "Open schedule Active" })).toBeTruthy();
});
