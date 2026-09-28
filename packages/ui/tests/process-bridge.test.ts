import { readFileSync } from "node:fs";
import { expect, test, vi } from "vitest";
import { invokeHost } from "../src/host/hostBridge";
import {
  listProcesses,
  readProcess,
  stopProcess,
  parseProcess,
  ProcessOutputBuffer,
  type ProcessPage,
} from "../src/lib/processBridge";
vi.mock("../src/host/hostBridge", () => ({ invokeHost: vi.fn() }));
const sample = JSON.parse(
  readFileSync(
    new URL(
      "../../runtime/generated/process-session-samples.json",
      import.meta.url,
    ),
    "utf8",
  ),
);
test("Host requests and responses match Rust process samples", async () => {
  expect(parseProcess(sample.agentSnapshot)).toEqual(sample.agentSnapshot);
  vi.mocked(invokeHost).mockResolvedValue(sample.list);
  expect(await listProcesses("session-sample")).toEqual(sample.list);
  expect(invokeHost).toHaveBeenLastCalledWith("process_session_list", {
    request: { sessionId: "session-sample" },
  });
  vi.mocked(invokeHost).mockResolvedValue(sample.output);
  expect(await readProcess("session-sample", "process-sample", "0")).toEqual(
    sample.output,
  );
  expect(invokeHost).toHaveBeenLastCalledWith("process_session_read", {
    request: sample.readRequest,
  });
  vi.mocked(invokeHost).mockResolvedValue(sample.snapshot);
  expect(await stopProcess("session-sample", "process-sample")).toEqual(
    sample.snapshot,
  );
  expect(invokeHost).toHaveBeenLastCalledWith("process_session_stop", {
    request: sample.target,
  });
  expect(() => parseProcess({ ...sample.snapshot, oldField: true })).toThrow();
  expect(() => parseProcess({ ...sample.snapshot, state: "done" })).toThrow();
  await expect(stopProcess("other", "process-sample")).rejects.toThrow(
    "owner mismatch",
  );
});
test("output handles split UTF-8, separate streams, gaps, controls and memory bounds", () => {
  const buffer = new ProcessOutputBuffer();
  const page = (
    bytes: number[],
    stream = "stdout",
    complete = false,
  ): ProcessPage => ({
    ...sample.output,
    process: { ...sample.snapshot, outputComplete: complete },
    chunks: [
      { cursor: "0", stream, dataBase64: btoa(String.fromCharCode(...bytes)) },
    ],
    hasMore: false,
  });
  expect(buffer.append(page([0xe4, 0xb8]))).toBe("");
  expect(buffer.append(page([65], "stderr"))).toBe("A");
  expect(buffer.append(page([0xad], "stdout", true))).toBe("A中");
  expect(buffer.append({ ...sample.output, gap: true })).toContain(
    "Earlier output",
  );
  expect(
    buffer.append(
      page([27, 91, 51, 49, 109, 88, 27, 91, 48, 109], "stdout", true),
    ),
  ).toMatch(/X$/);
  const large = {
    ...sample.output,
    chunks: [
      {
        cursor: "1",
        stream: "stdout" as const,
        dataBase64: btoa("z".repeat(300 * 1024)),
      },
    ],
  };
  expect(buffer.append(large).length).toBe(256 * 1024);
  expect(buffer.truncated).toBe(true);
});
