// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { TranscriptBlockList } from "../../src/chat/TranscriptBlockList";
import { createTranscriptViewStore } from "../../src/chat/transcriptViewStore";
import { TranscriptActivity } from "../../src/chat/TranscriptActivity";
import { WorkProgress } from "../../src/chat/WorkProgress";
import userEvent from "@testing-library/user-event";

vi.mock("../../src/i18n", () => {
  const t = (key: string, options?: { count?: number }) => options?.count === undefined ? key : `${key}:${options.count}`;
  return { t, useTranslation: () => ({ t }) };
});

afterEach(() => { cleanup(); vi.useRealTimers(); vi.unstubAllGlobals(); });

test("a final-only response never exposes or mounts an empty process disclosure", () => {
  const mounted = vi.fn();
  function Process() { mounted(); return <p>Process details</p>; }
  const { rerender } = render(<WorkProgress hasProcess={false} running finalStarted={false} response={<p>Streaming answer</p>}><Process /></WorkProgress>);
  expect(screen.queryByRole("button")).toBeNull();
  expect(screen.getByText("Streaming answer")).toBeTruthy();
  rerender(<WorkProgress hasProcess={false} running={false} finalStarted response={<p>Final answer</p>}><Process /></WorkProgress>);
  expect(screen.queryByRole("button")).toBeNull();
  expect(screen.getByText("Final answer")).toBeTruthy();
  expect(screen.queryByText("Process details")).toBeNull();
  expect(mounted).not.toHaveBeenCalled();
});

test("real process content remains expandable without hiding the final response", async () => {
  vi.stubGlobal("matchMedia", () => ({ matches: true }));
  const user = userEvent.setup();
  render(<WorkProgress hasProcess running={false} finalStarted response={<p>Final answer</p>}><p>Process details</p></WorkProgress>);
  const toggle = screen.getByRole("button", { name: "workProgress.worked" });
  expect(screen.queryByText("Process details")).toBeNull();
  await user.click(toggle);
  expect(screen.getByText("Process details")).toBeTruthy();
  expect(screen.getByText("Final answer")).toBeTruthy();
  await user.click(toggle);
  expect(screen.queryByText("Process details")).toBeNull();
  expect(screen.getByText("Final answer")).toBeTruthy();
});

test("activity moves below output while its work clock remains uninterrupted", async () => {
  vi.useFakeTimers(); vi.setSystemTime(10000);
  const props = { running: true, finalStarted: false, startedAtMs: 1000 };
  function View({ activity, running = true }: { activity: "thinking" | "working" | "waitingInput"; running?: boolean }) {
    return <><WorkProgress hasProcess={false} {...props} running={running} completedAtMs={running ? undefined : 14000} /><p>Latest output</p>{running ? <TranscriptActivity activity={activity} /> : null}</>;
  }
  const { container, rerender } = render(<View activity="thinking" />);
  expect(screen.getByRole("status").textContent).toBe("transcriptActivity.thinking");
  expect(screen.getByText("workProgress.working workProgress.seconds:9")).toBeTruthy();
  await act(async () => { vi.advanceTimersByTime(2000); });
  rerender(<View activity="working" />);
  expect(screen.getByText("workProgress.working workProgress.seconds:11")).toBeTruthy();
  expect(screen.getByRole("status").textContent).toBe("transcriptActivity.working");
  await act(async () => { vi.advanceTimersByTime(2000); });
  rerender(<View activity="waitingInput" />);
  expect(screen.getByText("workProgress.working workProgress.seconds:13")).toBeTruthy();
  expect(screen.getByRole("status").textContent).toBe("transcriptActivity.waitingInput");
  expect(container.lastElementChild).toBe(screen.getByRole("status"));
  rerender(<View activity="thinking" running={false} />);
  expect(screen.queryByRole("status")).toBeNull();
  expect(screen.getByText("workProgress.worked workProgress.seconds:13")).toBeTruthy();
});

test("delayed completion keeps the elapsed seconds already shown while history uses its recorded duration", async () => {
  vi.useFakeTimers(); vi.setSystemTime(10000);
  vi.stubGlobal("matchMedia", () => ({ matches: true }));
  const { rerender, unmount } = render(<WorkProgress hasProcess={false} running finalStarted={false} startedAtMs={1000} />);
  await act(async () => { vi.advanceTimersByTime(3000); });
  expect(screen.getByText("workProgress.working workProgress.seconds:12")).toBeTruthy();
  rerender(<WorkProgress hasProcess={false} running={false} finalStarted startedAtMs={1000} completedAtMs={11000} />);
  expect(screen.getByText("workProgress.worked workProgress.seconds:12")).toBeTruthy();
  await act(async () => { vi.advanceTimersByTime(5000); });
  expect(screen.getByText("workProgress.worked workProgress.seconds:12")).toBeTruthy();
  unmount();
  render(<WorkProgress hasProcess={false} running={false} finalStarted startedAtMs={1000} completedAtMs={11000} />);
  expect(screen.getByText("workProgress.worked workProgress.seconds:10")).toBeTruthy();
});

test.each(["failed", "cancelled", "completed"] as const)("%s submissions retain the original message and elapsed time without adding notices or actions", async (status) => {
  vi.useFakeTimers(); vi.setSystemTime(10000);
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const store = createTranscriptViewStore();
  const pending = { text: "Retain the exact uncommitted input", sessionId: null, agentRunId: "run", startedAtMs: 1000 };
  const props = { store, sessionId: null, loadingHistory: false, loadingOlderHistory: false, onLoadOlderHistory: async () => {} };
  const { container, rerender } = render(<TranscriptBlockList {...props} pendingUserMessage={pending} running />);
  expect(container.textContent).toContain("workProgress.working");
  await act(async () => { vi.advanceTimersByTime(2000); });
  rerender(<TranscriptBlockList {...props} pendingUserMessage={{ ...pending, status, completedAtMs: 12000 }} />);
  expect(screen.queryByRole("status")).toBeNull();
  expect(screen.queryByRole("button", { name: "agentChat.retry" })).toBeNull();
  expect(container.textContent).toContain(pending.text);
  expect(container.textContent).not.toContain("workProgress.working");
  const completed = container.querySelector(".workProgressHeader")!.textContent;
  expect(completed).toContain("workProgress.seconds:11");
  await act(async () => { vi.advanceTimersByTime(5000); });
  expect(container.querySelector(".workProgressHeader")!.textContent).toBe(completed);
});
