import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  TOOL_GROUP_CATEGORY_ORDER,
  formatToolGroupTitle,
  toolGroupIconCategory,
} from "../../src/chat/toolGroupTitle.ts";

const read = (language) => JSON.parse(readFileSync(new URL(`../../src/locales/${language}.json`, import.meta.url), "utf8"));
const locales = { en: read("en"), "zh-CN": read("zh-CN") };
const translator = (language) => (key) => locales[language][key];

const operation = (toolName, status = "completed", extra = {}) => ({
  toolName,
  description: "",
  target: "",
  status,
  ...extra,
});

test("categories merge into one clause and render in fixed display order", () => {
  const operations = [operation("bash"), operation("edit"), operation("bash")];
  assert.equal(formatToolGroupTitle(operations, translator("en")), "Edited 1 file, Ran 2 commands");
  assert.equal(formatToolGroupTitle(operations, translator("zh-CN")), "编辑了 1 个文件，运行了 2 个命令");
});

test("read is ordered before web search", () => {
  const operations = [operation("web_search"), operation("read")];
  assert.equal(formatToolGroupTitle(operations, translator("en")), "Read 1 file, Searched the web");
  assert.equal(formatToolGroupTitle(operations, translator("zh-CN")), "读取了 1 个文件，搜索了网页");
});

test("unknown tools are treated as commands without an external-tool category", () => {
  const operations = [operation("mcp__github__search"), operation("some-plugin-tool")];
  assert.equal(formatToolGroupTitle(operations, translator("en")), "Ran 2 commands");
  assert.doesNotMatch(formatToolGroupTitle(operations, translator("en")), /tool/i);
});

test("the published-artifact tool keeps its own category", () => {
  const operations = [operation("publish_artifact"), operation("bash")];
  assert.equal(formatToolGroupTitle(operations, translator("en")), "Published 1 artifact, Ran 1 command");
});

test("counts always use Arabic numerals, including a count of one", () => {
  assert.equal(formatToolGroupTitle([operation("bash")], translator("en")), "Ran 1 command");
  assert.equal(formatToolGroupTitle([operation("edit"), operation("write")], translator("en")), "Edited 2 files");
  assert.equal(formatToolGroupTitle([operation("bash")], translator("zh-CN")), "运行了 1 个命令");
  assert.equal(formatToolGroupTitle([operation("agent")], translator("en")), "Ran 1 agent");
});

test("a running operation makes the whole merged category progressive", () => {
  const operations = [operation("bash"), operation("bash", "running")];
  assert.equal(formatToolGroupTitle(operations, translator("en")), "Running 2 commands");
  assert.equal(formatToolGroupTitle(operations, translator("zh-CN")), "正在运行 2 个命令");
});

test("web search never carries a count", () => {
  const operations = [operation("web_search"), operation("web_search")];
  assert.equal(formatToolGroupTitle(operations, translator("en")), "Searched the web");
});

test("a single operation keeps the desktop inline shortcuts", () => {
  assert.equal(
    formatToolGroupTitle([operation("bash", "completed", { description: "Run the tests" })], translator("en")),
    "Run the tests",
  );
  assert.equal(
    formatToolGroupTitle([operation("write", "running", { target: "src/a.ts" })], translator("en")),
    "Editing src/a.ts",
  );
  assert.equal(
    formatToolGroupTitle([operation("read", "completed", { target: "src/a.ts" })], translator("zh-CN")),
    "读取了 src/a.ts",
  );
  assert.equal(
    formatToolGroupTitle([operation("web_search", "running")], translator("en")),
    "Searching the web",
  );
});

test("the header icon follows the first category in display order", () => {
  assert.equal(toolGroupIconCategory([operation("read"), operation("bash")]), "command");
  assert.equal(toolGroupIconCategory([operation("write"), operation("bash")]), "edit");
  assert.equal(toolGroupIconCategory([operation("web_search"), operation("read")]), "read");
  assert.equal(toolGroupIconCategory([operation("mcp__unknown")]), "command");
  assert.equal(toolGroupIconCategory([]), null);
  assert.deepEqual(TOOL_GROUP_CATEGORY_ORDER, [
    "edit",
    "publishArtifact",
    "command",
    "read",
    "webSearch",
    "agent",
    "taskOutput",
  ]);
});
