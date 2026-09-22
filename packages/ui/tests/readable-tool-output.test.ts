import { expect, test } from "vitest";
import { readableToolOutput, formatOperationInlineSummary } from "../src/components/chat/toolActivityTranscriptModel";
test("readable pages preserve UTF-8 content and strip only Core-declared framing", () => {
  const op = { contentStartByte: 5, contentByteLength: 8 };
  expect(readableToolOutput(op, "meta\nRead 中文\nfooter")).toBe("Read 中");
  expect(readableToolOutput(op, "meta\nRead 中文\nfooter", 0, true)).toBe("meta\nRead 中文\nfooter");
  expect(readableToolOutput({}, "Read user-authored text\nContinuation: user text")).toBe("Read user-authored text\nContinuation: user text");
});
test("a read title shows the target once and never falls back to Read read", () => {
  expect(formatOperationInlineSummary({ callId: "c", toolName: "read", taskId: "t", taskTitle: "read", status: "done", path: "README.md" }, "done")).toBe("README.md");
  expect(formatOperationInlineSummary({ callId: "c", toolName: "read", taskId: "t", taskTitle: "read", status: "done" }, "done")).not.toMatch(/read.*read/i);
});
