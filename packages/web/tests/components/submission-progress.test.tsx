// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { TranscriptBlockList } from "../../src/chat/TranscriptBlockList";
import { createTranscriptViewStore } from "../../src/chat/transcriptViewStore";

vi.mock("../../src/i18n", () => {
  const t = (key: string, options?: { count?: number }) => options?.count === undefined ? key : `${key}:${options.count}`;
  return { t, useTranslation: () => ({ t }) };
});

afterEach(() => { cleanup(); vi.useRealTimers(); vi.unstubAllGlobals(); });

test("terminal submissions keep their input, freeze elapsed time and expose an actionable retry", async () => {
  vi.useFakeTimers(); vi.setSystemTime(10000);
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const retry = vi.fn(), store = createTranscriptViewStore();
  const pending = { text: "Retain the exact uncommitted input", sessionId: null, agentRunId: "run", startedAtMs: 1000 };
  const props = { store, sessionId: null, loadingHistory: false, loadingOlderHistory: false, onLoadOlderHistory: async () => {}, onRetrySubmission: retry };
  const { container, rerender } = render(<TranscriptBlockList {...props} pendingUserMessage={pending} running />);
  expect(container.textContent).toContain("workProgress.working");
  await act(async () => { vi.advanceTimersByTime(2000); });
  rerender(<TranscriptBlockList {...props} pendingUserMessage={{ ...pending, status: "failed", completedAtMs: 12000 }} />);
  expect(screen.getByRole("status").textContent).toContain("appRoute.submissionFailed");
  expect(container.textContent).toContain(pending.text);
  expect(container.textContent).not.toContain("workProgress.working");
  const completed = container.querySelector(".workProgressHeader")!.textContent;
  expect(completed).toContain("workProgress.seconds:11");
  await act(async () => { vi.advanceTimersByTime(5000); });
  expect(container.querySelector(".workProgressHeader")!.textContent).toBe(completed);
  vi.useRealTimers();
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "agentChat.retry" }));
  expect(retry).toHaveBeenCalledOnce();
  rerender(<TranscriptBlockList {...props} pendingUserMessage={{ ...pending, status: "completed", completedAtMs: 12000 }} />);
  expect(screen.getByRole("status").textContent).toBe("appRoute.submissionCompleted");
  expect(screen.queryByRole("button", { name: "agentChat.retry" })).toBeNull();
});
