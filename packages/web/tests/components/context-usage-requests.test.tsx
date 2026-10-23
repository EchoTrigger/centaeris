// @vitest-environment jsdom
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { ContextUsagePicker } from "../../src/components/ContextUsagePicker";
import { apiJson } from "../../src/api";

vi.mock("../../src/i18n", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("../../src/api", () => ({ apiJson: vi.fn() }));
const request = vi.mocked(apiJson);
const response = (sessionId = "session", usedTokens = 1) => ({
  schema: "session.context_usage.v1", sessionId,
  contextUsage: { usedTokens, maxContextTokens: 100, usedPercentage: usedTokens },
});
async function settle() { await act(async () => { await Promise.resolve(); }); }
beforeEach(() => {
  vi.useFakeTimers();
  vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
  request.mockResolvedValue(response());
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.clearAllMocks(); vi.useRealTimers(); });

test("an active Run does not repeatedly read unchanged context usage while idle", async () => {
  render(<ContextUsagePicker sessionId="session" isRunning contextRevision="7" />);
  await settle();
  await act(async () => { await vi.advanceTimersByTimeAsync(60_000); });
  expect(request).toHaveBeenCalledTimes(1);
});

test("context revisions coalesce behind one in-flight read and repeated revisions do not reread", async () => {
  let finish!: (value: unknown) => void;
  request.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const view = render(<ContextUsagePicker sessionId="session" isRunning contextRevision="7" />);
  await settle();
  view.rerender(<ContextUsagePicker sessionId="session" isRunning contextRevision="8" />);
  view.rerender(<ContextUsagePicker sessionId="session" isRunning contextRevision="9" />);
  expect(request).toHaveBeenCalledTimes(1);
  await act(async () => { finish(response()); });
  await settle();
  expect(request).toHaveBeenCalledTimes(2);
  view.rerender(<ContextUsagePicker sessionId="session" isRunning contextRevision="9" />);
  await settle();
  expect(request).toHaveBeenCalledTimes(2);
});

test("a hidden tab defers context reads and performs one calibration on becoming visible", async () => {
  const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
  const view = render(<ContextUsagePicker sessionId="session" isRunning contextRevision="7" />);
  await settle();
  expect(request).not.toHaveBeenCalled();
  view.rerender(<ContextUsagePicker sessionId="session" isRunning contextRevision="8" />);
  view.rerender(<ContextUsagePicker sessionId="session" isRunning contextRevision="9" />);
  visibility.mockReturnValue("visible");
  await act(async () => { document.dispatchEvent(new Event("visibilitychange")); });
  expect(request).toHaveBeenCalledTimes(1);
  await act(async () => { await vi.advanceTimersByTimeAsync(60_000); });
  expect(request).toHaveBeenCalledTimes(1);
});

test("switching sessions cancels the old read and never installs an old response", async () => {
  let finish!: (value: unknown) => void;
  let oldSignal: AbortSignal | undefined;
  request.mockImplementationOnce((_path, options) => {
    oldSignal = options?.signal ?? undefined;
    return new Promise(resolve => { finish = resolve; });
  }).mockResolvedValue(response("other", 2));
  const view = render(<ContextUsagePicker sessionId="session" isRunning contextRevision="7" />);
  await settle();
  view.rerender(<ContextUsagePicker sessionId="other" isRunning contextRevision="1" />);
  await settle();
  expect(oldSignal?.aborted).toBe(true);
  await act(async () => { finish(response("session", 99)); });
  expect(view.container.textContent).toContain("2 / 100");
  expect(view.container.textContent).not.toContain("99 / 100");
});

test("a failed context read stays bounded until a new authoritative revision or visibility calibration", async () => {
  request.mockRejectedValueOnce(new Error("offline"));
  const view = render(<ContextUsagePicker sessionId="session" isRunning contextRevision="7" />);
  await settle();
  await act(async () => { await vi.advanceTimersByTimeAsync(60_000); });
  expect(request).toHaveBeenCalledTimes(1);
  view.rerender(<ContextUsagePicker sessionId="session" isRunning contextRevision="8" />);
  await settle();
  expect(request).toHaveBeenCalledTimes(2);
});
