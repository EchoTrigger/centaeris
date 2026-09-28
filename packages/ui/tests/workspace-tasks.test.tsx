import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { WorkspaceTasks } from "../src/components/WorkspaceTasks";
import {
  listProcesses,
  readProcess,
  stopProcess,
} from "../src/lib/processBridge";
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
vi.mock("../src/lib/processBridge", async (original) => ({
  ...(await original<object>()),
  listProcesses: vi.fn(),
  readProcess: vi.fn(),
  stopProcess: vi.fn(),
}));
const process = {
  processSessionId: "p1",
  sessionId: "s1",
  program: "bash",
  args: ["-c", "echo hello"],
  cwd: "root",
  source: "hostCommand" as const,
  state: "running" as const,
  exitCode: null,
  terminationReason: null,
  cleanupComplete: false,
  outputComplete: false,
  error: null,
};
let view: ReactTestRenderer;
beforeEach(() => {
  vi.useFakeTimers();
  vi.resetAllMocks();
  vi.mocked(listProcesses).mockResolvedValue({
    serviceInstanceId: "epoch",
    processes: [process],
  });
  vi.mocked(readProcess).mockResolvedValue({
    process: {
      ...process,
      state: "exited",
      outputComplete: true,
      cleanupComplete: true,
    },
    chunks: [{ cursor: "0", stream: "stdout", dataBase64: btoa("hello") }],
    nextCursor: "1",
    earliestCursor: "0",
    gap: false,
    hasMore: false,
  });
  vi.mocked(stopProcess).mockResolvedValue({ ...process, state: "stopping" });
});
afterEach(async () => {
  if (view) await act(async () => view.unmount());
  vi.useRealTimers();
});
test("lists session processes, reads output and only explicit stop terminates", async () => {
  await act(async () => {
    view = create(<WorkspaceTasks sessionId="s1" />);
  });
  expect(listProcesses).toHaveBeenCalledWith("s1");
  expect(JSON.stringify(view.toJSON())).toContain("hello");
  expect(readProcess).toHaveBeenCalledWith("s1", "p1", "0");
  const stop = view.root.findByProps({ "aria-label": "Stop p1" });
  await act(async () => stop.props.onClick());
  expect(stopProcess).toHaveBeenCalledExactlyOnceWith("s1", "p1");
  await act(async () => view.unmount());
  expect(stopProcess).toHaveBeenCalledTimes(1);
});
test("unmount ignores late output and does not stop a process", async () => {
  let resolve!: (value: Awaited<ReturnType<typeof readProcess>>) => void;
  vi.mocked(readProcess).mockImplementation(
    () =>
      new Promise((r) => {
        resolve = r;
      }),
  );
  await act(async () => {
    view = create(<WorkspaceTasks sessionId="s1" />);
  });
  await act(async () => view.unmount());
  await act(async () =>
    resolve({
      process,
      chunks: [],
      nextCursor: "0",
      earliestCursor: "0",
      gap: false,
      hasMore: false,
    }),
  );
  await act(async () => {
    await vi.advanceTimersByTimeAsync(5000);
  });
  expect(readProcess).toHaveBeenCalledTimes(1);
  expect(stopProcess).not.toHaveBeenCalled();
});
test("list errors remain visible and retry is offered", async () => {
  vi.mocked(listProcesses).mockRejectedValue(new Error("offline"));
  await act(async () => {
    view = create(<WorkspaceTasks sessionId="s1" />);
  });
  expect(JSON.stringify(view.toJSON())).toContain("offline");
  expect(view.root.findByProps({ "aria-label": "Refresh tasks" })).toBeTruthy();
});
test("selecting another process discards the previous pending output", async () => {
  const other = { ...process, processSessionId: "p2", args: ["other"] };
  vi.mocked(listProcesses).mockResolvedValue({
    serviceInstanceId: "epoch",
    processes: [process, other],
  });
  let resolve!: (value: Awaited<ReturnType<typeof readProcess>>) => void;
  vi.mocked(readProcess).mockImplementation((_session, id) =>
    id === "p1"
      ? new Promise((r) => {
          resolve = r;
        })
      : Promise.resolve({
          process: { ...other, outputComplete: true },
          chunks: [
            {
              cursor: "0",
              stream: "stdout",
              dataBase64: btoa("second output"),
            },
          ],
          nextCursor: "1",
          earliestCursor: "0",
          gap: false,
          hasMore: false,
        }),
  );
  await act(async () => {
    view = create(<WorkspaceTasks sessionId="s1" />);
  });
  await act(async () => view.root.findByProps({ title: "p2" }).props.onClick());
  await act(async () =>
    resolve({
      process,
      chunks: [
        {
          cursor: "0",
          stream: "stdout",
          dataBase64: btoa("late first output"),
        },
      ],
      nextCursor: "1",
      earliestCursor: "0",
      gap: false,
      hasMore: false,
    }),
  );
  expect(JSON.stringify(view.toJSON())).toContain("second output");
  expect(JSON.stringify(view.toJSON())).not.toContain("late first output");
});

 test("hidden task views suspend observation and resume without stopping", async () => {
  vi.mocked(readProcess).mockResolvedValue({ process, chunks: [], nextCursor: "0", earliestCursor: "0", gap: false, hasMore: false });
  await act(async () => { view = create(<WorkspaceTasks sessionId="s1" active={true} />); });
  await act(async () => view.update(<WorkspaceTasks sessionId="s1" active={false} />));
  const calls = vi.mocked(listProcesses).mock.calls.length;
  const reads = vi.mocked(readProcess).mock.calls.length;
  await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
  expect(listProcesses).toHaveBeenCalledTimes(calls);
  expect(readProcess).toHaveBeenCalledTimes(reads);
  await act(async () => view.update(<WorkspaceTasks sessionId="s1" active={true} />));
  expect(listProcesses).toHaveBeenCalledTimes(calls + 1);
  expect(stopProcess).not.toHaveBeenCalled();
});
