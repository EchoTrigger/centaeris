import assert from "node:assert/strict";
import { test } from "vitest";
import {
  getToolActivityAtom,
  getToolActivityPresentation,
} from "../src/components/chat/toolActivityModel.ts";

const result = (callId, toolName, fields = {}) => ({
  callId,
  toolName,
  status: "done",
  resultState: "successWithOutput",
  ...fields,
});

const edit = result("call-edit", "edit", {
  path: "ui/App.tsx",
  diffPreview: "--- a/ui/App.tsx\n+++ b/ui/App.tsx",
});
const read = result("call-read", "read", { path: "ui/App.tsx" });
const write = result("call-write", "write", {
  path: "ui/generated.ts",
  diffPreview: "--- /dev/null\n+++ ui/generated.ts\n+generated",
});
const command = result("call-bash", "bash", {
  kind: "command",
  outputPreview: "test result: ok",
  exitCode: 0,
});
const webSearch = result("call-search", "web_search", { matchCount: 1 });

test("binds title, icon, and detail to first-occurrence atom order", () => {
  const presentation = getToolActivityPresentation([command, read, edit]);
  assert.deepEqual(
    presentation.atoms.map((atom) => atom.kind),
    ["command", "read", "edit"],
  );
  assert.equal(presentation.title, "Ran commands · Read files · Edited files");
  assert.equal(presentation.iconToken, "command");
  assert.equal(presentation.atoms[0].detailRendererKind, "bash");

  const searchThenCommand = getToolActivityPresentation([webSearch, command]);
  assert.deepEqual(
    searchThenCommand.atoms.map((atom) => atom.kind),
    ["webSearch", "command"],
  );
  assert.equal(searchThenCommand.title, "Searched the web · Ran commands");
  assert.equal(searchThenCommand.iconToken, "webSearch");
});

test("deduplicates repeated atoms without reordering operation details", () => {
  const secondRead = { ...read, callId: "call-read-2", path: "ui/main.tsx" };
  const presentation = getToolActivityPresentation([read, secondRead, command]);
  assert.deepEqual(
    presentation.atoms.map((atom) => atom.kind),
    ["read", "command"],
  );
  assert.equal(presentation.title, "Read files · Ran commands");
  assert.equal(presentation.iconToken, "read");
});

test("presents successful and failed reads through the same model", () => {
  assert.equal(getToolActivityPresentation([read]).title, "Read files");
  const failedRead = result("call-read-failed", "read", {
    status: "error",
    resultState: "failed",
    path: "ui/missing.tsx",
    error: "not found",
  });
  assert.equal(getToolActivityPresentation([failedRead]).title, "Read files");
});

test("keeps settled failures visible and never requires a fake edit diff", () => {
  const failedEdit = result("call-edit-failed", "edit", {
    status: "error",
    resultState: "failed",
    path: "ui/App.tsx",
    error: "File must be read before mutation",
  });
  const presentation = getToolActivityPresentation([failedEdit]);
  assert.equal(presentation.atoms[0].detailRendererKind, "diff");
  assert.equal(presentation.title, "Edited files");

  assert.throws(
    () => getToolActivityPresentation([{ ...edit, diffPreview: undefined }]),
    /成功 edit operation 缺少 diffPreview/,
  );
  assert.throws(
    () => getToolActivityPresentation([{ ...write, diffPreview: undefined }]),
    /成功 write operation 缺少 diffPreview/,
  );
  assert.throws(
    () => getToolActivityPresentation([{ ...failedEdit, diffPreview: "-not-applied" }]),
    /失败 edit operation 不得携带 diffPreview/,
  );
});

test("uses exact toolName, presents canonical dynamic tools, and rejects damaged names", () => {
  assert.equal(getToolActivityAtom(command).kind, "command");
  assert.equal(getToolActivityPresentation([write]).title, "Wrote files");
  assert.equal(getToolActivityAtom(write).detailRendererKind, "diff");
  assert.throws(
    () => getToolActivityAtom({ ...read, kind: "read" }),
    /工具 operation kind 不支持: read\/read/,
  );
  assert.throws(
    () => getToolActivityAtom({ ...command, kind: undefined }),
    /工具 operation kind 不支持: bash\/<missing>/,
  );
  const dynamic = getToolActivityAtom(result("call-weather", "get_weather"));
  assert.equal(dynamic.kind, "externalTool");
  assert.equal(dynamic.title, "Ran external tools");
  assert.throws(
    () => getToolActivityAtom(result("call-bad", "GetWeather")),
    /不支持的工具 operation: GetWeather/,
  );
});
