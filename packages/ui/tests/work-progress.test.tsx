import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, expect, test, vi } from "vitest";
import { WorkProgress } from "../src/components/chat/WorkProgress";
import { formatWorkDuration } from "../src/components/chat/workDuration";
import { t } from "../src/i18n";

let renderer: ReactTestRenderer;
afterEach(() => { act(() => renderer?.unmount()); vi.useRealTimers(); });

test("duration omits zero units without padding", () => {
  expect(formatWorkDuration(0, t)).toBe("");
  expect(formatWorkDuration(20_000, t)).toBe("20s");
  expect(formatWorkDuration(80_000, t)).toBe("1m 20s");
  expect(formatWorkDuration(3_600_000, t)).toBe("1h");
  expect(formatWorkDuration(3_601_000, t)).toBe("1h 1s");
});

test("work starts before content, folds on answer, preserves supplements and stops on completion", () => {
  vi.useFakeTimers(); vi.setSystemTime(20_000);
  const render = (running: boolean, answer: boolean, extra = false) => <WorkProgress
    running={running} finalStarted={answer} startedAtMs={0} completedAtMs={running ? undefined : 80_000}
  ><p>Process</p>{extra ? <p>Supplement</p> : null}</WorkProgress>;
  act(() => { renderer = create(render(true, false)); });
  const button = () => renderer.root.findByType("button");
  expect(renderer.root.findAllByType("button")).toHaveLength(0);
  expect(renderer.root.findByProps({ className: "workProgressStatus" }).children).toEqual(["Working 20s"]);
  act(() => { vi.setSystemTime(80_000); renderer.update(render(true, false, true)); vi.advanceTimersByTime(1000); });
  expect(renderer.root.findByProps({ className: "workProgressStatus" }).children).toEqual(["Working 1m 21s"]);
  expect(renderer.root.findAllByType("p")).toHaveLength(2);
  act(() => renderer.update(render(true, true, true)));
  expect(button().props["aria-expanded"]).toBe(false);
  expect(renderer.root.findAllByType("p")).toHaveLength(0);
  act(() => renderer.update(render(false, true, true)));
  expect(button().props["aria-label"]).toBe("Worked 1m 20s");
  act(() => button().props.onClick());
  expect(renderer.root.findAllByType("p")).toHaveLength(2);
  act(() => { vi.advanceTimersByTime(60_000); });
  expect(button().props["aria-label"]).toBe("Worked 1m 20s");
});


test("historical process, supplements and final share one disclosure between user turns", async () => {
  const { transcriptMessageGroups } = await import("../src/components/chat/transcriptMessageGroups");
  const messages = [
    { id: "u1", role: "user" },
    { id: "reason", role: "assistant", turn: {} },
    { id: "supplement", role: "assistant", turn: {} },
    { id: "final", role: "assistant", turn: {} },
    { id: "u2", role: "user" },
    { id: "live", role: "assistant", turn: { agentRunId: "run:2" } },
  ] as import("../src/components/chat/types").ChatMessage[];
  expect(transcriptMessageGroups(messages.map((message) => message.id), (id) => messages.find((message) => message.id === id)))
    .toEqual([["u1"], ["reason", "supplement", "final"], ["u2"], ["live"]]);
});
