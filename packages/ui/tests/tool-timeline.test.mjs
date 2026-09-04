import assert from "node:assert/strict";
import { test } from "vitest";
import * as model from "../src/components/chat/toolTimelineModel.ts";
import { normalizeToolOperation } from "../src/components/chat/chatToolRuntimeModel.ts";

const command = {
  callId: "call-bash",
  toolName: "bash",
  kind: "command",
  status: "done",
  resultState: "successWithOutput",
  outputPreview: "test result: ok",
  exitCode: 0,
  normalizedInput: { command: "cargo test | rg fail" },
};

test("reads exact command and path from the paired ToolCall", () => {
  assert.equal(model.isCommandOperation(command), true);
  assert.equal(model.getOperationCommand(command), "cargo test | rg fail");
  assert.equal(model.formatCompactCommandLine("cargo test"), "$ cargo test");
  assert.equal(model.formatFullCommandLine("cargo test"), "$ cargo test");

  const read = {
    callId: "call-read",
    toolName: "read",
    status: "done",
    resultState: "successWithOutput",
    startLine: 1,
    endLine: 1689,
    totalLines: 1987,
    normalizedInput: { path: "ui/App.tsx" },
  };
  assert.equal(model.getOperationPath(read), "ui/App.tsx");
  assert.equal(model.isOperationPathOpenable(read), true);
  assert.equal(model.formatOperationLineCoverage(read), "lines 1-1689 of 1987");
});

test("normalizes the result operation without runtime title or non-bash kind", () => {
  const read = {
    callId: "call-read",
    toolName: "read",
    status: "ok",
    resultState: "successWithOutput",
    path: "docs/example.md",
    startLine: 1,
    endLine: 20,
    totalLines: 42,
  };
  assert.deepEqual(
    Object.fromEntries(
      Object.entries(normalizeToolOperation(read)).filter(([, value]) => value !== undefined),
    ),
    read,
  );
  assert.throws(
    () => normalizeToolOperation({ ...read, title: "Read file" }),
    /不支持旧 title/,
  );
  assert.throws(
    () => normalizeToolOperation({ ...read, kind: "read" }),
    /工具 operation kind 不支持: read\/read/,
  );
  assert.throws(
    () => normalizeToolOperation({ ...command, normalizedInput: undefined, kind: "read" }),
    /工具 operation kind 不支持: bash\/read/,
  );
});
