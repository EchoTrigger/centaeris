import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { test } from "vitest";
import {
  buildTranscriptProcessViewModel,
} from "../src/components/chat/agentTranscriptModel.ts";
import {
  extractToolResultSpillContent,
  formatCompletedToolGroupTitle,
  formatRunningToolGroupTitle,
} from "../src/components/chat/toolActivityTranscriptModel.ts";

test("extracts the exact UTF-8 tool result body from a spill", () => {
  const prefix = '{"description":"check"}\n--- tool result ---\n';
  const result = "HEAD界TAIL";
  const spill = `${prefix}${result}\n--- tool result metadata ---\n{}`;
  assert.equal(
    extractToolResultSpillContent(
      spill,
      new TextEncoder().encode(prefix).length,
      new TextEncoder().encode(result).length,
    ),
    result,
  );
});

const makeTask = ({ id, operations, status = "done", title, turnId }) => {
  const first = operations[0] || {};
  const normalizedInput = {
    ...(typeof first.inputCommand === "string" ? { command: first.inputCommand } : {}),
    ...(typeof first.path === "string" ? { path: first.path } : {}),
    ...(typeof first.query === "string" ? { query: first.query } : {}),
  };
  return {
    id,
    operations: operations.map(({ inputCommand: _command, ...operation }) => ({
      ...operation,
      callId: operation.callId || id,
      kind: operation.toolName === "bash" && status !== "running" ? "command" : undefined,
      status,
      resultState: status === "running" ? undefined : operation.resultState || "successWithOutput",
    })),
    normalizedInput,
    provider: "tool",
    status,
    summary: "",
    title,
    turnId,
  };
};

test("keeps reads in their chronological position before and after completion", () => {
  const command = makeTask({
    id: "command-a",
    turnId: "turn-a",
    title: "bash",
    operations: [
      {
        toolName: "bash",
        kind: "command",
        inputCommand: "echo a",
        outputPreview: "a",
        exitCode: 0,
      },
    ],
  });
  const read = makeTask({
    id: "read-b",
    turnId: "turn-b",
    title: "read",
    operations: [
      {
        toolName: "read",
        path: "ui/App.tsx",
      },
    ],
  });
  const runningRead = makeTask({
    id: "read-c",
    turnId: "turn-c",
    title: "read",
    operations: [
      {
        toolName: "read",
        path: "ui/main.tsx",
      },
    ],
    status: "running",
  });
  const chunks = [
    { id: "chunk-a", kind: "task", task: command },
    { id: "summary-a", kind: "narrative", text: "阶段总结一" },
    { id: "chunk-b", kind: "task", task: read },
    { id: "summary-b", kind: "narrative", text: "阶段总结二" },
    { id: "chunk-c", kind: "task", task: runningRead },
  ];

  const live = buildTranscriptProcessViewModel({ chunks, isStreaming: true });
  assert.deepEqual(
    live.processItems.map((item) => `${item.kind}:${item.id}`),
    [
      "toolGroup:command-a-activity",
      "assistantText:summary-a",
      "toolGroup:read-b-activity",
      "assistantText:summary-b",
      "toolGroup:read-c-activity",
    ],
  );

  const terminal = buildTranscriptProcessViewModel({ chunks, isStreaming: false });
  assert.deepEqual(
    terminal.processItems.map((item) => `${item.kind}:${item.id}`),
    [
      "toolGroup:command-a-activity",
      "assistantText:summary-a",
      "toolGroup:read-b-activity",
      "assistantText:summary-b",
      "toolGroup:read-c-activity",
    ],
  );
});

test("keeps one active tool header while its members settle", () => {
  const settledRead = makeTask({
    id: "settled-read",
    turnId: "turn-shared",
    title: "read",
    operations: [
      {
        toolName: "read",
        path: "ui/App.tsx",
        startLine: 1,
        endLine: 20,
        totalLines: 42,
      },
    ],
  });
  const runningCommand = makeTask({
    id: "running-command",
    turnId: "turn-shared",
    title: "bash",
    status: "running",
    operations: [
      {
        toolName: "bash",
        kind: "command",
        inputCommand: "Get-Content ui/App.tsx",
      },
    ],
  });
  const view = buildTranscriptProcessViewModel({
    isStreaming: true,
    chunks: [
      { id: "settled-read", kind: "task", task: settledRead },
      { id: "running-command", kind: "task", task: runningCommand },
    ],
  });

  assert.equal(view.processItems.length, 1);
  assert.equal(view.processItems[0]?.id, "settled-read-activity");
  assert.deepEqual(
    view.processItems[0]?.tasks.map((task) => task.id),
    ["settled-read", "running-command"],
  );

  const completedCommand = makeTask({
    id: "running-command",
    turnId: "turn-shared",
    title: "bash",
    operations: [
      {
        toolName: "bash",
        inputCommand: "Get-Content ui/App.tsx",
      },
    ],
  });
  const settledView = buildTranscriptProcessViewModel({
    isStreaming: true,
    chunks: [
      { id: "settled-read", kind: "task", task: settledRead },
      { id: "running-command", kind: "task", task: completedCommand },
    ],
  });

  assert.equal(settledView.processItems[0]?.id, view.processItems[0]?.id);
  assert.deepEqual(
    settledView.processItems[0]?.tasks.map((task) => task.id),
    ["settled-read", "running-command"],
  );
});

test("preserves completed read-only groups around assistant stages", () => {
  const makeRead = (id, turnId) =>
    makeTask({
      id,
      turnId,
      title: "read",
      operations: [
        {
          toolName: "read",
          path: `${id}.ts`,
          startLine: 1,
          endLine: 10,
          totalLines: 20,
        },
      ],
    });
  const view = buildTranscriptProcessViewModel({
    isStreaming: false,
    chunks: [
      { id: "read-a", kind: "task", task: makeRead("read-a", "turn-a") },
      { id: "read-b", kind: "task", task: makeRead("read-b", "turn-b") },
      { id: "summary", kind: "narrative", text: "阶段总结" },
      { id: "read-c", kind: "task", task: makeRead("read-c", "turn-b") },
    ],
  });

  assert.deepEqual(
    view.processItems.map((item) => `${item.kind}:${item.id}`),
    ["toolGroup:read-a-activity", "assistantText:summary", "toolGroup:read-c-activity"],
  );
});

test("uses exact structured operations and leaves no legacy summary chain", () => {
  const command = makeTask({
    id: "command",
    turnId: "turn-combined",
    title: "bash",
    operations: [
      {
        toolName: "bash",
        kind: "command",
        inputCommand: "cargo test | rg fail",
        outputPreview: "ok",
        exitCode: 0,
      },
    ],
  });
  const edit = makeTask({
    id: "edit",
    turnId: "turn-combined",
    title: "edit",
    operations: [
      {
        toolName: "edit",
        path: "ui/App.tsx",
        diffPreview: "--- ui/App.tsx\n+++ ui/App.tsx",
      },
    ],
  });
  const read = makeTask({
    id: "read",
    title: "read",
    operations: [
      {
        toolName: "read",
        path: "ui/App.tsx",
      },
    ],
  });
  const web = makeTask({
    id: "web",
    title: "web_search",
    operations: [
      {
        toolName: "web_search",
        query: "Centaeris",
        matchCount: 1,
      },
    ],
  });
  const weather = makeTask({
    id: "weather",
    title: "get_weather",
    operations: [
      { toolName: "get_weather" },
    ],
  });

  assert.equal(formatCompletedToolGroupTitle([command]), "Ran a command");
  assert.equal(
    formatCompletedToolGroupTitle([web, command, read, edit]),
    "Searched the web, Ran a command, Read a file, Edited a file",
  );
  const runningCommand = makeTask({
    id: "running-command",
    title: "bash",
    status: "running",
    operations: [{ toolName: "bash", inputCommand: "cargo test" }],
  });
  assert.equal(
    formatRunningToolGroupTitle([runningCommand]),
    "Running a command",
  );
  assert.equal(formatCompletedToolGroupTitle([weather]), "Ran an external tool");
  for (const isStreaming of [true, false]) {
    const item = buildTranscriptProcessViewModel({
      isStreaming,
      chunks: [{ id: "weather", kind: "task", task: weather }],
    }).processItems[0];
    assert.equal(item?.kind, "toolGroup");
    assert.equal(item?.tasks[0]?.operations[0]?.toolName, "get_weather");
  }
  assert.equal(
    formatRunningToolGroupTitle([{
      ...weather,
      status: "running",
      operations: weather.operations.map((operation) => ({
        ...operation,
        status: "running",
        resultState: undefined,
      })),
    }]),
    "Running an external tool",
  );
  assert.throws(
    () => formatCompletedToolGroupTitle([{
      ...weather,
      operations: weather.operations.map((operation) => ({
        ...operation,
        toolName: "get-weather",
      })),
    }]),
    /不支持的工具 operation: get-weather/,
  );

  const firstGroup = buildTranscriptProcessViewModel({
    isStreaming: false,
    chunks: [{ id: "command", kind: "task", task: command }],
  }).processItems[0];
  const expandedGroup = buildTranscriptProcessViewModel({
    isStreaming: false,
    chunks: [
      { id: "command", kind: "task", task: command },
      { id: "edit", kind: "task", task: edit },
    ],
  }).processItems[0];
  assert.equal(firstGroup.id, expandedGroup.id);

});

test("keeps command titles bounded inside the process text rail", async () => {
  const rootDir = path.resolve(import.meta.dirname, "..");
  const chatStyles = await readFile(
    path.join(rootDir, "src", "styles", "chat.css"),
    "utf8",
  );

  assert.match(
    chatStyles,
    /\.agent-tool-node\s*\{[^}]*width:\s*100%;[^}]*min-width:\s*0;[^}]*max-width:\s*100%;/,
  );
  assert.match(
    chatStyles,
    /\.agent-tool-node-summary\s*\{[^}]*display:\s*flex;[^}]*width:\s*100%;[^}]*min-width:\s*0;[^}]*max-width:\s*100%;[^}]*overflow:\s*hidden;/,
  );
  assert.match(
    chatStyles,
    /\.agentProcessFeed\s*\{[^}]*width:\s*var\(--agent-text-rail\);[^}]*margin-left:\s*auto;[^}]*margin-right:\s*auto;/,
  );
  assert.doesNotMatch(
    chatStyles,
    /\.agentProcessFeed\s*\{[^}]*padding-left:\s*14px;/,
  );
  assert.match(
    chatStyles,
    /\.agent-tool-node-summary\s*\{[^}]*padding:\s*8px 12px;/,
  );
  assert.match(
    chatStyles,
    /\.agent-tool-node-list\s*\{[^}]*border:\s*1px solid[^}]*border-radius:\s*10px;/,
  );
  assert.match(
    chatStyles,
    /\.agent-tool-node-action\.is-inline-summary\s*\{[^}]*flex:\s*0 1 auto;[^}]*min-width:\s*0;[^}]*max-width:\s*100%;[^}]*overflow:\s*hidden;[^}]*text-overflow:\s*ellipsis;[^}]*white-space:\s*nowrap;/,
  );
  assert.doesNotMatch(
    chatStyles,
    /execution-board|tool-drawer-|tool-timeline-card|agent-activity-section/,
  );
  assert.match(
    chatStyles,
    /\.agent-tool-bash-status\s*\{[^}]*justify-self:\s*end;/,
  );
  assert.match(
    chatStyles,
    /\.agent-tool-bash-scroll\s*\{[^}]*contain:\s*layout paint;/,
  );
  assert.doesNotMatch(chatStyles, /\.agent-tool-bash-scroll\s*\{[^}]*mask-image:/);
  assert.match(
    chatStyles,
    /animation:\s*centaerisRunStatusWave 4s ease-in-out infinite;/,
  );
  assert.match(chatStyles, /0%\s*\{[^}]*background-position:\s*120% 50%;/);
  assert.match(chatStyles, /75%,\s*\n\s*100%\s*\{[^}]*background-position:\s*-120% 50%;/);
  assert.doesNotMatch(chatStyles, /agentProcessSummary|AgentProcessHeaderText/);
});
