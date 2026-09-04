import assert from "node:assert/strict";
import { test } from "vitest";
import * as restore from "../src/components/chat/chatTranscriptRestore.ts";
import * as runtimeCore from "../src/components/chat/chatRuntimeCore.ts";
import * as runtimeModel from "../src/components/chat/chatRuntimeModel.ts";

const eventPayload = (event) => ({
  type: "session_event",
  agentRunId: event.agentRunId || "agent-run-restore",
  cursor: event.cursor ?? 0,
  event: {
    version: "v1",
    sessionId: "session-restore",
    turnId: "turn-restore",
    taskId: event.agentRunId || "agent-run-restore",
    parentTaskId: "turn-restore",
    visibility: "user",
    at: event.at,
    status: event.status || "done",
    ...event,
  },
});

test("restores a canonical dynamic tool operation", () => {
  const turn = restore.buildAssistantTurnFromStreamItems(
    [
      eventPayload({
        id: "evt-weather-call",
        type: "ToolCall",
        toolName: "get_weather",
        status: "running",
        payload: {
          callId: "call-weather",
          normalizedInput: { location: "Taipei" },
          displayTarget: "Taipei",
        },
      }),
      eventPayload({
        id: "evt-weather-result",
        type: "ToolResult",
        toolName: "get_weather",
        payload: {
          callId: "call-weather",
          resultState: "successWithOutput",
          operations: [{
            callId: "call-weather",
            toolName: "get_weather",
            status: "ok",
            resultState: "successWithOutput",
            outputPreview: "25 C",
          }],
        },
      }),
    ],
    "",
    0,
  );

  const task = turn.chunks.find((chunk) => chunk.kind === "task")?.task;
  assert.equal(task?.title, "get_weather");
  assert.equal(task?.operations?.[0]?.toolName, "get_weather");
});

test("restores the chat runtime contract", async () => {
  assert.equal(runtimeCore.mapProcessStateToActivity(undefined), null);
  assert.throws(
    () => runtimeCore.mapProcessStateToActivity("unknown"),
    /未知 runtime processState: unknown/,
  );
  assert.deepEqual(runtimeCore.mapProcessStateToActivity("searching"), {
    kind: "thinking",
    label: "Thinking",
    processState: "searching",
  });
  assert.deepEqual(runtimeCore.mapProcessStateToActivity("executing"), {
    kind: "executing",
    label: "Thinking",
    processState: "executing",
  });
  assert.deepEqual(runtimeCore.mapProcessStateToActivity("compressing"), {
    kind: "compressing",
    label: "Compacting",
    processState: "compressing",
  });
  assert.deepEqual(runtimeCore.mapPreparingToolNameToActivity("write"), {
    kind: "executing",
    label: "Editing",
  });
  assert.deepEqual(runtimeCore.mapPreparingToolNameToActivity("bash"), {
    kind: "executing",
    label: "Preparing",
  });
  assert.deepEqual(runtimeCore.mapPreparingToolNameToActivity("web_search"), {
    kind: "thinking",
    label: "Searching",
  });
  assert.deepEqual(
    runtimeCore.mapPreparingToolNameToActivity("banana"),
    runtimeCore.DEFAULT_RUNTIME_ACTIVITY,
  );
  assert.throws(
    () => runtimeCore.mapProcessStateToActivity("banana"),
    /未知 runtime processState: banana/,
  );
  assert.throws(
    () => restore.normalizePersistedContent([{ content: "banana" }]),
    /message content 必须是 string/,
  );
  assert.throws(
    () => restore.resolvePersistedTaskStatus("completed"),
    /session_event status 不支持: completed/,
  );
  assert.equal(
    runtimeCore.formatRuntimeModelError({ message: "resolve path failed: banana" }),
    "resolve path failed: banana",
  );
  const diagnosticOperation = runtimeModel.normalizeToolOperation({
    callId: "call-diagnostic",
    toolName: "read",
    status: "error",
    resultState: "failed",
    error: "file_read_range_already_covered: banana",
  });
  assert.equal(diagnosticOperation.status, "error");
  assert.equal(diagnosticOperation.error, "file_read_range_already_covered: banana");

  const tachikomaAgentRunId = "tachikoma-0";
  assert.equal(runtimeCore.tachikomaEasterEgg(tachikomaAgentRunId, 2, 2), null);
  assert.equal(runtimeCore.tachikomaEasterEgg(tachikomaAgentRunId, 3, 3), 3);
  assert.equal(runtimeCore.tachikomaEasterEgg(tachikomaAgentRunId, 3, 1), 1);
  assert.equal(
    runtimeCore.runtimeEasterEgg("runtime-0", "thinking"),
    "a faint signal crossed the Wired…",
  );

  const turn = restore.buildAssistantTurnFromStreamItems(
    [
      eventPayload({
        id: "evt-process",
        type: "Status",
        status: "running",
        processState: "reading",
        at: 100,
        payload: {
          stage: "model_process_summary",
          message: "正在检查仓库结构",
        },
      }),
      eventPayload({
        id: "evt-tool-call",
        type: "ToolCall",
        toolName: "bash",
        status: "running",
        at: 150,
        payload: {
          callId: "call-bash",
          command: "cargo test",
          description: "Run focused tests",
          displayTarget: "Run focused tests",
        },
      }),
      eventPayload({
        id: "evt-tool",
        type: "ToolResult",
        toolName: "bash",
        status: "done",
        processState: "executing",
        at: 200,
        payload: {
          callId: "call-bash",
          resultState: "successWithOutput",
          summary: "运行命令",
          latencyMs: 1234,
          operations: [
            {
              callId: "call-bash",
              toolName: "bash",
              kind: "command",
              status: "ok",
              resultState: "successWithOutput",
              outputPreview: "test result: ok",
              exitCode: 0,
            },
          ],
        },
      }),
      eventPayload({
        id: "evt-failed-tool-call",
        type: "ToolCall",
        toolName: "edit",
        status: "running",
        at: 225,
        payload: {
          callId: "call-edit",
          normalizedInput: {
            path: "src/lib.rs",
            edits: [{ old_text: "old", new_text: "new" }],
          },
          displayTarget: "src/lib.rs",
        },
      }),
      eventPayload({
        id: "evt-failed-tool",
        type: "ToolResult",
        toolName: "edit",
        status: "error",
        processState: "executing",
        at: 250,
        payload: {
          callId: "call-edit",
          resultState: "failed",
          summary: "编辑失败",
          operations: [
            {
              callId: "call-edit",
              toolName: "edit",
              status: "error",
              resultState: "failed",
              path: "src/lib.rs",
              error: "patch failed",
            },
          ],
        },
      }),
      eventPayload({
        id: "evt-final",
        type: "Final",
        at: 400,
        payload: {
          content: "最终回答",
        },
      }),
    ],
    "",
    0,
  );

  assert.equal(turn.finalAnswer, "最终回答");
  assert.equal(turn.startedAtMs, 100);
  assert.equal(turn.completedAtMs, 400);

  const narrativeChunks = turn.chunks.filter(
    (chunk) => chunk.kind === "narrative",
  );
  assert.ok(
    narrativeChunks.some((chunk) => chunk.text === "正在检查仓库结构"),
    "durable model process summary must survive restore",
  );
  assert.ok(
    !narrativeChunks.some((chunk) => chunk.text === "会话已停止。"),
    "stopped process narrative should not render in main chat",
  );
  assert.ok(
    !narrativeChunks.some((chunk) => chunk.text.includes("patch failed")),
    "tool-scoped failures must stay inside the ToolResult task",
  );

  const taskChunks = turn.chunks.filter((chunk) => chunk.kind === "task");
  assert.equal(taskChunks.length, 2);
  const bashTask = taskChunks.find(
    (chunk) => chunk.task.title === "bash",
  )?.task;
  assert.ok(bashTask, "Bash task should restore");
  assert.equal(bashTask.status, "done");
  assert.equal(bashTask.durationMs, 1234);
  assert.equal(bashTask.normalizedInput?.command, "cargo test");
  assert.equal(bashTask.normalizedInput?.description, "Run focused tests");
  assert.equal(bashTask.displayTarget, "Run focused tests");
  assert.equal(bashTask.operations?.[0]?.outputPreview, "test result: ok");

  const editTask = taskChunks.find(
    (chunk) => chunk.task.title === "edit",
  )?.task;
  assert.ok(editTask, "failed Edit task should restore");
  assert.equal(editTask.status, "error");
  assert.equal(editTask.operations?.[0]?.kind, undefined);
  assert.equal(editTask.operations?.[0]?.diffPreview, undefined);
  assert.equal(editTask.operations?.[0]?.error, "patch failed");

  assert.throws(
    () =>
      restore.buildAssistantTurnFromStreamItems(
        [
          eventPayload({
            id: "evt-missing-call-id",
            type: "ToolResult",
            toolName: "bash",
            payload: { operations: [] },
          }),
        ],
        "",
        0,
      ),
    /ToolResult 缺少 callId/,
  );
  assert.throws(
    () =>
      restore.buildAssistantTurnFromStreamItems(
        [
          eventPayload({
            id: "evt-legacy-operations-alias-call",
            type: "ToolCall",
            toolName: "bash",
            payload: {
              callId: "call-legacy-operations-alias",
              normalizedInput: { command: "cargo test" },
              displayTarget: "cargo test",
            },
          }),
          eventPayload({
            id: "evt-legacy-operations-alias",
            type: "ToolResult",
            toolName: "bash",
            payload: {
              callId: "call-legacy-operations-alias",
              operationsJson: "[]",
            },
          }),
        ],
        "",
        0,
      ),
    /payload\.operations 必须是 array/,
  );
  assert.throws(
    () =>
      restore.buildAssistantTurnFromStreamItems(
        [
          eventPayload({
            id: "evt-operation-missing-tool-name-call",
            type: "ToolCall",
            toolName: "bash",
            payload: {
              callId: "call-operation-missing-tool-name",
              normalizedInput: { command: "cargo test" },
              displayTarget: "cargo test",
            },
          }),
          eventPayload({
            id: "evt-operation-missing-tool-name",
            type: "ToolResult",
            toolName: "bash",
            payload: {
              callId: "call-operation-missing-tool-name",
              operations: [{
                callId: "call-operation-missing-tool-name",
                kind: "command",
                status: "ok",
                resultState: "successWithOutput",
              }],
            },
          }),
        ],
        "",
        0,
      ),
    /toolName 必须是 canonical lower_snake_case/,
  );

  const processSummaryDuplicateTurn = restore.buildAssistantTurnFromStreamItems(
    [
      eventPayload({
        id: "evt-model-delta-dup-1",
        type: "ModelTextDelta",
        at: 600,
        payload: { delta: "好问题。让我直接验证。" },
      }),
      eventPayload({
        id: "evt-process-summary-dup",
        type: "Status",
        at: 602,
        payload: {
          stage: "model_process_summary",
          message: "好问题。让我直接验证。",
        },
      }),
      eventPayload({
        id: "evt-tool-call-after-summary",
        type: "ToolCall",
        toolName: "bash",
        at: 603,
        payload: {
          callId: "call-after-summary",
          summary: "运行命令",
        },
      }),
    ],
    "",
    0,
  );
  const restoredProcessNarratives = processSummaryDuplicateTurn.chunks.filter(
    (chunk) =>
      chunk.kind === "narrative" && chunk.text === "好问题。让我直接验证。",
  );
  assert.equal(
    restoredProcessNarratives.length,
    1,
    "model_process_summary must not add restored narrative beyond streamed assistant text",
  );

  const durableProcessTurn = restore.buildAssistantTurnFromStreamItems(
    [
      eventPayload({
        id: "evt-process-summary-1",
        type: "Status",
        at: 700,
        payload: {
          stage: "model_process_summary",
          message: "先准备 Rust 环境。",
        },
      }),
      eventPayload({
        id: "evt-tool-call-process-1",
        type: "ToolCall",
        toolName: "bash",
        at: 701,
        payload: {
          callId: "call-process-1",
          summary: "检查 Rust",
        },
      }),
      eventPayload({
        id: "evt-tool-result-process-1",
        type: "ToolResult",
        toolName: "bash",
        status: "done",
        at: 702,
        payload: {
          callId: "call-process-1",
          summary: "Rust 可用",
          operations: [],
        },
      }),
      eventPayload({
        id: "evt-process-summary-2",
        type: "Status",
        at: 703,
        payload: {
          stage: "model_process_summary",
          message: "接着创建游戏前端。",
        },
      }),
      eventPayload({
        id: "evt-tool-call-process-2",
        type: "ToolCall",
        toolName: "write",
        at: 704,
        payload: {
          callId: "call-process-2",
          summary: "写入前端文件",
        },
      }),
    ],
    "",
    0,
  );
  assert.deepEqual(
    durableProcessTurn.chunks.map((chunk) => chunk.kind),
    ["narrative", "task", "narrative", "task"],
    "durable process summaries must preserve tool-group boundaries",
  );
  assert.deepEqual(
    durableProcessTurn.chunks
      .filter((chunk) => chunk.kind === "narrative")
      .map((chunk) => chunk.text),
    ["先准备 Rust 环境。", "接着创建游戏前端。"],
  );

  assert.equal(
    runtimeModel.buildRestoreTurn(null, null),
    null,
    "checkpoint state must not fabricate a restored assistant message",
  );

  let cancelled = false;
  let yieldCount = 0;
  const manyMessages = Array.from({ length: 14 }, (_item, index) => ({
    id: `msg-user-${index}`,
    role: "user",
    content: `message ${index}`,
    createdAtMs: index + 1,
    updatedAtMs: index + 1,
  }));
  const chunkedMessages = await runtimeModel.buildHistoryMessagesChunked(
    "",
    { messages: manyMessages },
    new Map(),
    new Map(),
    {
      isCancelled: () => cancelled,
      yieldToUi: async () => {
        yieldCount += 1;
      },
    },
  );
  assert.equal(chunkedMessages.length, manyMessages.length);
  assert.ok(yieldCount > 0, "chunked hydration should yield between batches");

  const inlineTerminalMessage = await runtimeModel.buildHistoryMessagesChunked(
    "",
    {
      messages: [
        {
          id: "assistant-inline-terminal",
          role: "assistant",
          content: "final answer",
          status: "done",
          agentRunId: "agent-run-no-durable-stream",
        },
      ],
    },
    new Map([["agent-run-no-durable-stream", []]]),
    new Map([
      [
        "agent-run-no-durable-stream",
        {
          agentRunId: "agent-run-no-durable-stream",
          sessionId: "chat-restore",
          status: "succeeded",
        },
      ],
    ]),
  );
  assert.equal(inlineTerminalMessage.length, 1);
  assert.equal(inlineTerminalMessage[0].role, "assistant");
  assert.equal(inlineTerminalMessage[0].turn.finalAnswer, "final answer");

  const failedMessage = await runtimeModel.buildHistoryMessagesChunked(
    "",
    {
      messages: [{
        id: "assistant-failed",
        role: "assistant",
        content: "durable tool call commit failed",
        status: "error",
        turnId: "turn-failed",
        agentRunId: "agent-run-failed",
      }],
    },
    new Map([["agent-run-failed", [eventPayload({
      id: "evt-failed-final",
      type: "Final",
      status: "error",
      payload: { content: "durable tool call commit failed" },
    })]]]),
    new Map([["agent-run-failed", {
      agentRunId: "agent-run-failed",
      sessionId: "session-restore",
      status: "failed",
    }]]),
  );
  assert.equal(failedMessage[0].turn.finalAnswer, "");
  assert.deepEqual(
    failedMessage[0].turn.chunks
      .filter((chunk) => chunk.kind === "narrative")
      .map((chunk) => chunk.text),
    ["durable tool call commit failed"],
  );

  cancelled = false;
  await assert.rejects(
    runtimeModel.buildHistoryMessagesChunked(
      "",
      { messages: manyMessages },
      new Map(),
      new Map(),
      {
        isCancelled: () => cancelled,
        yieldToUi: async () => {
          cancelled = true;
        },
      },
    ),
    /历史恢复已取消/,
  );

});
