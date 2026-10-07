import { coreFile } from '../../../../scripts/core-source.mjs';
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { WorkspaceChatController } from "../../src/chat/workspaceChatController.ts";

import { createChatViewStore } from "../../src/chat/chatViewStore.ts";
import { isAgentRunActive, validateHistoryPage, applyStreamEntry } from "../../src/chat/sessionEvents.ts";
import { reasoningPreview } from "../../src/chat/reasoningPreview.ts";
import { buildAgentRunSections, formatPhaseElapsed, runningActivityPresentation, toolAtom } from "../../src/chat/agentRunPresentation.mjs";

test("tool copy can change language without translating a model-supplied description", () => {
  const translate = (label) => ({ "Running a command": "正在运行命令", "Read files": "已读取文件" })[label] || label;
  assert.equal(runningActivityPresentation({ toolName: "bash", status: "running", call: {} }, translate).label, "正在运行命令");
  assert.equal(runningActivityPresentation({ toolName: "bash", status: "running", call: { normalizedInput: { description: "Running a command" } } }, translate).label, "Running a command");
  assert.equal(toolAtom("read", translate).title, "已读取文件");
});

test("action icons follow the agreed cross-client vocabulary", () => {
  for (const [tool, icon] of Object.entries({ read: "search", web_search: "globe", bash: "terminal", write: "edit", edit: "edit", agent: "agent", task_output: "listChecks", publish_artifact: "fileOutput" })) {
    assert.equal(toolAtom(tool).icon, icon);
  }
});

test("reasoning separates tool groups in sequence order with stable identities", () => {
  const tool = (id, sequence) => ({ activityId: id, sequence, toolName: "read", status: "completed", call: {} });
  const activities = [tool("c", 4), tool("a", 1), tool("b", 2)];
  const reasoning = [
    { id: "r2", sequence: 5, text: "Check", status: "done" },
    { id: "r1", sequence: 3, text: "Inspect", status: "streaming" },
  ];
  const before = structuredClone({ activities, reasoning });
  const sections = buildAgentRunSections([], activities, reasoning);
  const items = sections.flatMap((section) => section.items);
  assert.deepEqual(items.map((item) => item.kind), ["toolGroup", "reasoning", "toolGroup", "reasoning"]);
  assert.deepEqual(items[0].group.activityIds, ["a", "b"]);
  assert.deepEqual(items[2].group.activityIds, ["c"]);
  const updated = buildAgentRunSections([], activities, reasoning.map((block) => ({ ...block, text: `${block.text} more`, status: "done" })));
  assert.deepEqual(updated.flatMap((section) => section.items).map((item) => item.kind === "reasoning" ? item.block.id : item.group.activityIds[0]), ["a", "r1", "c", "r2"]);
  assert.deepEqual({ activities, reasoning }, before);
});

const sessionId = "session_1";
const agentRunId = "agent_run_1";

test("platform snapshots hydrate without legacy events and stale or foreign responses cannot replace them", () => {
  const citation = { citationId: "citation:1", inputRef: "input", displayName: "Material", sourceToolCallId: "call", sourceUrl: "/api/citations/citation:1" };
  const run = validateHistoryPage(page(historyAgentRun([], { citations: [citation], citationSequence: 4 }))).agentRuns[0];
  assert.deepEqual(run.citations, [citation]);
  const store = createChatViewStore();
  store.replaceAll([run]);
  const snapshot = { schema: "workspace.citations.v1", agentRunId, sessionId, throughSequence: 3, citations: [] };
  store.applyCitationSnapshot(snapshot);
  store.applyCitationSnapshot({ ...snapshot, sessionId: "foreign", throughSequence: 5 });
  assert.deepEqual(store.getAgentRunSnapshot(agentRunId).citations, [citation]);
  store.replaceAgentRun({ ...run, citations: [], citationSequence: 2 });
  assert.deepEqual(store.getAgentRunSnapshot(agentRunId).citations, [citation]);
  store.applyCitationSnapshot({ ...snapshot, throughSequence: 5 });
  assert.deepEqual(store.getAgentRunSnapshot(agentRunId).citations, []);
  assert.equal(store.getAgentRunSnapshot(agentRunId).streamCursor, run.streamCursor);
});

test("live snapshots recover from history, ignore stale revisions and transition to one sealed block", () => {
  const reasoning = JSON.parse(readFileSync(coreFile("packages/core/tests/fixtures/live_reasoning.json"), "utf8"));
  assert.equal(reasoningPreview(reasoning.text), "核对 input 保留 code 与 来源");
  const live = (revision, text) => liveEntry(revision, text, null, { turnId: "turn_1", messageId: "answer", reasoning });
  let view = validateHistoryPage(page(historyAgentRun([]))).agentRuns[0];
  view = applyStreamEntry(view, live(2, "answer"));
  assert.equal(view.reasoningBlocks[0].text, reasoning.text);
  assert.equal(view.reasoningBlocks[0].status, "streaming");
  assert.ok(view.reasoningBlocks[0].sequence < view.messages[0].sequence);
  assert.equal(applyStreamEntry(view, live(1, "old")).messages[0].text, "answer");
  const restored = validateHistoryPage(page(historyAgentRun([], { live: view.live }))).agentRuns[0];
  assert.deepEqual(restored.reasoningBlocks, view.reasoningBlocks);
  view = applyStreamEntry(view, committedEntry(event("reasoning_block", 3, { ...reasoning, status: "interrupted" })));
  assert.equal(view.reasoningBlocks.length, 1);
  assert.equal(view.reasoningBlocks[0].status, "interrupted");
  view = applyStreamEntry(view, live(3, "answer"));
  assert.equal(view.reasoningBlocks[0].status, "interrupted");
  assert.equal(view.reasoningBlocks.length, 1);
  const invalid = live(4, "answer"); invalid.item.reasoning = { ...reasoning, durationMs: 1 };
  assert.throws(() => applyStreamEntry(view, invalid));
});

test("committed reasoning survives history reconstruction and separates tools", () => {
  const payload = JSON.parse(readFileSync(coreFile("packages/core/tests/fixtures/reasoning_block.json"), "utf8"));
  const reasoning = event("reasoning_block", 3, payload);
  const history = page(historyAgentRun([reasoning]));
  const view = validateHistoryPage(history).agentRuns[0];
  assert.deepEqual(view.reasoningBlocks, [{ id: payload.blockId, turnId: "turn_1", sequence: 3, text: " inspect\n", status: "done" }]);
  assert.deepEqual(validateHistoryPage(history).agentRuns[0].reasoningBlocks, view.reasoningBlocks);
  const liveView = validateHistoryPage(page(historyAgentRun([reasoning], { live: { messageId: "message:turn_1:assistant", turnId: "turn_1", afterSequence: 2, revision: 1, text: "Answer" } }))).agentRuns[0];
  assert.ok(liveView.messages[0].sequence > liveView.reasoningBlocks[0].sequence, "the same call's live answer follows its committed reasoning");
  assert.equal(validateHistoryPage(page(historyAgentRun([reasoning, event("reasoning_block", 4, reasoning.event.payload)]))).agentRuns[0].projectionError, "session_projection_invalid");
  for (const payload of [{ ...reasoning.event.payload, status: "streaming" }, { ...reasoning.event.payload, durationMs: 10 }, { ...reasoning.event.payload, blockId: "other" }]) {
    assert.equal(validateHistoryPage(page(historyAgentRun([event("reasoning_block", 3, payload)]))).agentRuns[0].projectionError, "session_projection_invalid");
  }
});

function event(type, sequence, payload, turnId = "turn_1") {
  return {
    sequence,
    event: {
      schemaVersion: "session.event.v1",
      eventVersion: 1,
      sequence,
      type,
      eventId: `event:${sequence}`,
      sessionId: sessionId,
      turnId,
      agentRunId: agentRunId,
      createdAtMs: sequence,
      payload,
    },
  };
}

function historyAgentRun(events, overrides = {}) {
  return {
    id: agentRunId,
    status: "running",
    model: { id: "model_1", displayName: "Model" },
    createdAt: "2026-08-15T00:00:00Z",
    startedAt: "2026-08-15T00:00:00Z",
    completedAt: null,
    events,
    live: null,
    streamCursor: "1-0",
    citations: [],
    citationSequence: 0,
    ...overrides,
  };
}

function page(agentRun) {
  return {
    schema: "session.history.page.v1",
    session: { id: sessionId, workspaceId: "workspace_1" },
    agentRuns: Array.isArray(agentRun) ? agentRun : [agentRun],
    nextCursor: null,
    hasMore: false,
  };
}

function liveEntry(revision, text, cursor = `${revision}-0`, overrides = {}) {
  return {
    cursor,
    item: {
      schema: "session.stream.item.v1",
      kind: "live",
      agentRunId,
      afterSequence: 2,
      revision,
      turnId: "turn_2",
      messageId: "message:turn_2:assistant",
      text,
      ...overrides,
    },
  };
}

function committedEntry(stored, cursor = `${stored.sequence}-0`) {
  return {
    cursor,
    item: {
      schema: "session.stream.item.v1",
      kind: "committed",
      agentRunId,
      sourceSequence: stored.sequence,
      event: stored.event,
    },
  };
}

test("history keeps assistant, tools, and final answer in source order", () => {
  const agentRun = validateHistoryPage(page(historyAgentRun([
    event("agent_run_started", 1, { userObjective: "检查仓库" }),
    event("user_message", 2, { messageId: "message:user", text: "检查仓库", attachments: [] }),
    event("phase_event", 3, { stage: "model_process_summary", message: "我先读取，再修改。" }),
    event("tool_call", 4, { callId: "read_1", toolName: "read", toolContractDigest: `sha256:${"a".repeat(64)}`, providerId: "centaeris.builtin", normalizedInput: { path: "a.txt" }, displayTarget: "a.txt" }),
    event("tool_result", 5, { callId: "read_1", toolName: "read", resultState: "successWithOutput", modelContent: "a", summary: "read", latencyMs: 1, operations: [{ callId: "read_1", toolName: "read", status: "ok", resultState: "successWithOutput", path: "a.txt" }] }),
    event("tool_call", 6, { callId: "edit_1", toolName: "edit", toolContractDigest: `sha256:${"b".repeat(64)}`, providerId: "centaeris.builtin", normalizedInput: { path: "a.txt" }, displayTarget: "a.txt" }),
    event("tool_result", 7, { callId: "edit_1", toolName: "edit", resultState: "successNoOutput", modelContent: "", summary: "edited", latencyMs: 1, operations: [{ callId: "edit_1", toolName: "edit", status: "ok", resultState: "successNoOutput", path: "a.txt", diffPreview: "-a\n+b" }] }),
    event("assistant_message", 8, { messageId: "message:final", modelMarkdown: "完成。", artifactRefs: [], status: "done" }, "turn_2"),
  ]))).agentRuns[0];

  const sections = buildAgentRunSections(agentRun.messages, agentRun.activities);
  assert.deepEqual(sections.map((section) => section.turnId), ["turn_1", "turn_2"]);
  assert.equal(sections[0].items[0].group.presentation.title, "Read files · Edited files");
  assert.equal(sections[0].items[0].group.presentation.icon, "search");
  assert.deepEqual(agentRun.activities.map((activity) => activity.toolName), ["read", "edit"]);
  assert.equal(Object.hasOwn(agentRun.activities[0], "action"), false);
  assert.equal(agentRun.activities[0].call.normalizedInput.path, "a.txt");
  assert.equal(agentRun.activities[1].result.operations[0].diffPreview, "-a\n+b");
  assert.equal(agentRun.messages.at(-1).text, "完成。");
});

test("tool projection keeps one group while a tool is running", () => {
  const agentRun = validateHistoryPage(page(historyAgentRun([
    event("agent_run_started", 1, { userObjective: "inspect" }),
    event("user_message", 2, { messageId: "message:user", text: "inspect", attachments: [] }),
    event("tool_call", 3, { callId: "read_1", toolName: "read", toolContractDigest: `sha256:${"a".repeat(64)}`, providerId: "centaeris.builtin", normalizedInput: { path: "a" }, displayTarget: "a" }),
    event("tool_result", 4, { callId: "read_1", toolName: "read", resultState: "successNoOutput", modelContent: "", summary: "read", latencyMs: 1, operations: [] }),
    event("tool_call", 5, { callId: "bash_1", toolName: "bash", toolContractDigest: `sha256:${"b".repeat(64)}`, providerId: "centaeris.builtin", normalizedInput: { command: "pwd" }, displayTarget: "pwd" }, "turn_2"),
  ]))).agentRuns[0];

  const sections = buildAgentRunSections(agentRun.messages, agentRun.activities);
  assert.equal(sections.length, 1);
  assert.equal(sections[0].items[0].group.presentation.title, "Read files · Ran commands");
  assert.equal(sections[0].items[0].group.status, "running");
  assert.equal(sections[0].items[0].group.activities.length, 2);
  assert.equal(sections[0].sectionId, buildAgentRunSections(agentRun.messages, agentRun.activities.slice(0, 1))[0].sectionId);
  assert.deepEqual(runningActivityPresentation(agentRun.activities[1]), { icon: "terminal", label: "Running a command" });
  assert.deepEqual(runningActivityPresentation({
    ...agentRun.activities[1],
    call: { normalizedInput: { command: "pwd", description: "Inspect workspace" }, displayTarget: "Inspect workspace" },
  }), { icon: "terminal", label: "Inspect workspace" });
  assert.throws(() => toolAtom("banana"), /unsupported tool activity: banana/);
});

test("activity disclosures survive row recycling and refresh without changing run facts", () => {
  const store = createChatViewStore();
  const agentRun = validateHistoryPage(page(historyAgentRun([]))).agentRuns[0];
  store.replaceAll([agentRun]);
  const facts = store.getAgentRunSnapshot(agentRunId);
  let changes = 0;
  let renders = 0;
  store.subscribeChanges(() => { changes += 1; });
  const unsubscribe = store.subscribeAgentRun(agentRunId, () => { renders += 1; });
  store.toggleActivityDisclosure(agentRunId, "activity:read_1");
  store.toggleActivityDisclosure(agentRunId, "operation:read_1:0");
  assert.equal(store.getAgentRunSnapshot(agentRunId), facts);
  assert.equal(changes, 0);
  assert.equal(renders, 2);
  const expanded = store.getActivityDisclosures(agentRunId);
  unsubscribe();
  store.replaceAgentRun(agentRun);
  store.replaceAll([agentRun]);
  assert.equal(store.getActivityDisclosures(agentRunId), expanded);
  assert.deepEqual([...expanded], ["activity:read_1", "operation:read_1:0"]);
  store.toggleActivityDisclosure(agentRunId, "activity:read_1");
  assert.deepEqual([...store.getActivityDisclosures(agentRunId)], ["operation:read_1:0"]);
  store.replaceAll([]);
  store.appendAgentRun(agentRun);
  assert.equal(store.getActivityDisclosures(agentRunId).size, 0);
  store.toggleActivityDisclosure(agentRunId, "activity:read_1");
  store.clear();
  assert.equal(store.getActivityDisclosures(agentRunId).size, 0);
});

test("contracted dynamic tools present as commands without weakening built-in loud-fail", () => {
  const agentRun = validateHistoryPage(page(historyAgentRun([
    event("agent_run_started", 1, { userObjective: "research" }),
    event("user_message", 2, { messageId: "message:user", text: "research", attachments: [] }),
    event("tool_call", 3, {
      callId: "banana_1",
      toolName: "banana_fetch",
      toolContractDigest: `sha256:${"c".repeat(64)}`,
      providerId: "mcp:banana:banana",
      normalizedInput: { title: "上海市中小学手机管理规范" },
      displayTarget: "上海市中小学手机管理规范",
    }),
  ]))).agentRuns[0];

  const group = buildAgentRunSections(agentRun.messages, agentRun.activities)[0].items[0].group;
  assert.equal(group.presentation.title, "Ran commands");
  assert.deepEqual(runningActivityPresentation(agentRun.activities[0]), {
    icon: "terminal",
    label: "Running 上海市中小学手机管理规范",
  });
  assert.throws(() => runningActivityPresentation({
    ...agentRun.activities[0],
    call: { ...agentRun.activities[0].call, providerId: "centaeris.builtin" },
  }), /unsupported tool activity: banana_fetch/);
  const quarantined = validateHistoryPage(page(historyAgentRun([
    event("tool_call", 1, { ...agentRun.activities[0].call, toolContractDigest: "banana" }),
  ]))).agentRuns[0];
  assert.equal(quarantined.projectionError, "session_projection_invalid");
  assert.deepEqual(quarantined.activities, []);
});

test("one malformed AgentRun is quarantined without hiding a valid dynamic tool run", () => {
  const validRunId = "agent_run_2";
  const validToolEvent = event("tool_call", 1, {
    callId: "banana_2",
    toolName: "banana_fetch",
    toolContractDigest: `sha256:${"d".repeat(64)}`,
    providerId: "mcp:banana:banana",
    normalizedInput: { title: "上海市校规" },
    displayTarget: "上海市校规",
  });
  validToolEvent.event.agentRunId = validRunId;
  const projected = validateHistoryPage(page([
    historyAgentRun([event("banana", 1, {})]),
    historyAgentRun([validToolEvent], { id: validRunId }),
  ])).agentRuns;

  assert.equal(projected[0].projectionError, "session_projection_invalid");
  assert.equal(projected[1].projectionError, null);
  assert.equal(projected[1].activities[0].toolName, "banana_fetch");
});

test("terminal failure stays inspectable without inventing a red recovery message", () => {
  const failed = validateHistoryPage(page(historyAgentRun([
    event("agent_run_failed", 1, {
      reasonType: "runtime_error",
      message: "Bearer banana at http://runtime.internal",
    }),
  ], { status: "failed", completedAt: "2026-08-15T00:00:01Z" }))).agentRuns[0];

  assert.equal(failed.status, "failed");
  assert.equal(failed.failureReason, "runtime_error");
  assert.equal(failed.errorMessage, undefined);
});

test("display state machine groups only consecutive tools between assistant text", () => {
  const agentRun = validateHistoryPage(page(historyAgentRun([
    event("agent_run_started", 1, { userObjective: "inspect" }),
    event("user_message", 2, { messageId: "message:user", text: "inspect", attachments: [] }),
    event("tool_call", 3, { callId: "read_1", toolName: "read", toolContractDigest: `sha256:${"a".repeat(64)}`, providerId: "centaeris.builtin", normalizedInput: { path: "a" }, displayTarget: "a" }),
    event("tool_result", 4, { callId: "read_1", toolName: "read", resultState: "successNoOutput", modelContent: "", summary: "read", latencyMs: 1, operations: [] }),
    event("tool_call", 5, { callId: "read_2", toolName: "read", toolContractDigest: `sha256:${"a".repeat(64)}`, providerId: "centaeris.builtin", normalizedInput: { path: "b" }, displayTarget: "b" }, "turn_2"),
    event("tool_result", 6, { callId: "read_2", toolName: "read", resultState: "successNoOutput", modelContent: "", summary: "read", latencyMs: 1, operations: [] }, "turn_2"),
    event("phase_event", 7, { stage: "model_process_summary", message: "先看完这两处。" }, "turn_2"),
    event("tool_call", 8, { callId: "bash_1", toolName: "bash", toolContractDigest: `sha256:${"b".repeat(64)}`, providerId: "centaeris.builtin", normalizedInput: { command: "pwd" }, displayTarget: "pwd" }, "turn_2"),
    event("tool_result", 9, { callId: "bash_1", toolName: "bash", resultState: "successWithOutput", modelContent: "/mnt/data", summary: "pwd", latencyMs: 1, operations: [] }, "turn_2"),
    event("assistant_message", 10, { messageId: "message:final", modelMarkdown: "完成。", artifactRefs: [], status: "done" }, "turn_3"),
  ]))).agentRuns[0];

  const sections = buildAgentRunSections(agentRun.messages, agentRun.activities);
  assert.deepEqual(sections.map((section) => [section.message?.text || null, section.items.map((item) => item.group.activities.length)]), [
    [null, [2]],
    ["先看完这两处。", [1]],
    ["完成。", []],
  ]);
  assert.equal(sections[1].items[0].group.presentation.title, "Ran a command");
});

test("all settled supported tool activity remains in history", () => {
  const publish = {
    activityId: "activity:publish_1", callId: "publish_1", toolName: "publish_artifact",
    turnId: "turn_1", sequence: 1, status: "completed", call: { normalizedInput: { path: "report.md" } }, result: {},
  };
  assert.equal(buildAgentRunSections([], [publish])[0].items[0].group.presentation.title, "Published artifacts");

  const failedRead = {
    activityId: "activity:read_1", callId: "read_1", toolName: "read",
    turnId: "turn_3", sequence: 3, status: "failed", call: { normalizedInput: { path: "banana" } }, result: {},
  };
  assert.equal(buildAgentRunSections([], [failedRead])[0].items[0].group.presentation.title, "Read files");
});

test("committed compaction restores one lightweight timeline marker", () => {
  const agentRun = validateHistoryPage(page(historyAgentRun([
    event("agent_run_started", 1, { userObjective: "inspect" }),
    event("user_message", 2, { messageId: "message:user", text: "inspect", attachments: [] }),
    event("compaction", 3, {
      compactionId: "compaction-1",
      summaryMessageId: "summary-1",
      summaryMarkdown: "summary",
      firstKeptMessageId: null,
      createdReason: "context_pressure_threshold_reached",
    }),
  ]))).agentRuns[0];

  assert.equal(agentRun.messages.at(-1).phase, "compaction");
  assert.equal(agentRun.messages.at(-1).text, "Compacted conversation");
  assert.equal(buildAgentRunSections(agentRun.messages, agentRun.activities).at(-1).message.phase, "compaction");
});

test("live text is a replace-only overlay and disappears at terminal commit", () => {
  const initial = validateHistoryPage(page(historyAgentRun([
    event("agent_run_started", 1, { userObjective: "hello" }),
    event("user_message", 2, { messageId: "message:user", text: "hello", attachments: [] }),
  ]))).agentRuns[0];
  const store = createChatViewStore();
  store.replaceAll([initial]);
  store.applyStreamEntries([liveEntry(1, "旧")]);
  store.applyStreamEntries([liveEntry(2, "新的完整正文", "2-0")]);
  assert.equal(store.getAgentRunSnapshot(agentRunId).messages.at(-1).text, "新的完整正文");

  store.applyStreamEntries([{
    cursor: "3-0",
    item: {
      schema: "session.stream.item.v1",
      kind: "committed",
      agentRunId,
      sourceSequence: 3,
      event: event("assistant_message", 3, { messageId: "message:turn_2:assistant", modelMarkdown: "最终正文", artifactRefs: [], status: "done" }, "turn_2").event,
    },
  }, {
    cursor: "4-0",
    item: {
      schema: "session.stream.item.v1",
      kind: "committed",
      agentRunId,
      sourceSequence: 4,
      event: event("agent_run_completed", 4, { doneReason: "stop" }).event,
    },
  }]);
  const terminal = store.getAgentRunSnapshot(agentRunId);
  assert.equal(terminal.status, "completed");
  assert.equal(terminal.connection, "completed");
  assert.equal(terminal.live, null);
  assert.equal(terminal.messages.at(-1).text, "最终正文");
});

test("controller batches entries and rejects post-terminal data", async () => {
  let citationRefreshes = 0;
  const store = createChatViewStore();
  store.replaceAll([validateHistoryPage(page(historyAgentRun([]))).agentRuns[0]]);
  const frames = [];
  const controller = new WorkspaceChatController({
    store,
    workspaceId: "workspace_1",
    sessionId,
    agentRunId,
    scheduleFrame: (callback) => (frames.push(callback), frames.length),
    cancelFrame: () => {},
    now: () => 0,
    onCitationsChanged: () => { citationRefreshes += 1; },
  });
  controller.accept(liveEntry(1, "正文"));
  const terminal = {
    cursor: "2-0",
    item: {
      schema: "session.stream.item.v1",
      kind: "committed",
      agentRunId,
      sourceSequence: 1,
      event: event("agent_run_failed", 1, { reasonType: "runtime_error", message: "失败" }).event,
    },
  };
  controller.accept(terminal);
  assert.throws(() => controller.accept(liveEntry(2, "banana")), /after terminal/);
  frames.shift()();
  await controller.whenIdle();
  assert.equal(store.getAgentRunSnapshot(agentRunId).status, "failed");
  assert.equal(citationRefreshes, 1);
});

test("committed phase atomically supersedes its older live overlay", () => {
  const store = createChatViewStore();
  store.replaceAll([validateHistoryPage(page(historyAgentRun([
    event("agent_run_started", 1, { userObjective: "inspect" }),
    event("user_message", 2, { messageId: "message:user", text: "inspect", attachments: [] }),
  ]))).agentRuns[0]]);
  store.applyStreamEntries([liveEntry(1, "same phase")]);
  assert.deepEqual(store.getAgentRunSnapshot(agentRunId).messages.map((message) => message.text), ["inspect", "same phase"]);

  store.applyStreamEntries([committedEntry(
    event("phase_event", 3, { stage: "model_process_summary", message: "same phase" }, "turn_2"),
  )]);

  const projected = store.getAgentRunSnapshot(agentRunId);
  assert.equal(projected.live, null);
  assert.equal(projected.overlayBarrierByTurnId.turn_2, 3);
  assert.deepEqual(projected.messages.map((message) => [message.phase, message.text]), [
    ["user", "inspect"],
    ["stage", "same phase"],
  ]);
});

test("late live older than a committed phase barrier never reappears", () => {
  const phase = event("phase_event", 3, { stage: "model_process_summary", message: "committed" }, "turn_2");
  const store = createChatViewStore();
  store.replaceAll([validateHistoryPage(page(historyAgentRun([
    event("agent_run_started", 1, { userObjective: "inspect" }),
    event("user_message", 2, { messageId: "message:user", text: "inspect", attachments: [] }),
    phase,
  ]))).agentRuns[0]]);

  store.applyStreamEntries([liveEntry(2, "late", "4-0", { afterSequence: 2 })]);
  const projected = store.getAgentRunSnapshot(agentRunId);
  assert.equal(projected.live, null);
  assert.deepEqual(projected.messages.filter((message) => message.role === "assistant").map((message) => message.text), ["committed"]);
});

test("live anchored exactly at the phase barrier is a new generation and remains visible", () => {
  const store = createChatViewStore();
  store.replaceAll([validateHistoryPage(page(historyAgentRun([
    event("agent_run_started", 1, { userObjective: "inspect" }),
    event("user_message", 2, { messageId: "message:user", text: "inspect", attachments: [] }),
    event("phase_event", 3, { stage: "model_process_summary", message: "first phase" }, "turn_2"),
  ]))).agentRuns[0]]);

  store.applyStreamEntries([liveEntry(1, "next generation", "4-0", { afterSequence: 3 })]);
  const projected = store.getAgentRunSnapshot(agentRunId);
  assert.equal(projected.live.afterSequence, 3);
  assert.deepEqual(projected.messages.filter((message) => message.role === "assistant").map((message) => message.text), [
    "first phase",
    "next generation",
  ]);
});

test("unrelated committed events do not advance or clear the live overlay", () => {
  const store = createChatViewStore();
  store.replaceAll([validateHistoryPage(page(historyAgentRun([
    event("agent_run_started", 1, { userObjective: "inspect" }),
    event("user_message", 2, { messageId: "message:user", text: "inspect", attachments: [] }),
  ]))).agentRuns[0]]);
  store.applyStreamEntries([liveEntry(1, "keep me")]);
  store.applyStreamEntries([committedEntry(event("tool_call", 3, {
    callId: "read_1",
    toolName: "read",
    toolContractDigest: `sha256:${"a".repeat(64)}`,
    providerId: "centaeris.builtin",
    normalizedInput: { path: "a" },
    displayTarget: "a",
  }, "turn_2"))]);

  const projected = store.getAgentRunSnapshot(agentRunId);
  assert.equal(projected.live.text, "keep me");
  assert.equal(projected.overlayBarrierByTurnId.turn_2, undefined);
  assert.equal(projected.messages.at(-1).text, "keep me");
});

test("identical committed phase text in different turns remains distinct", () => {
  const projected = validateHistoryPage(page(historyAgentRun([
    event("phase_event", 1, { stage: "model_process_summary", message: "same" }, "turn_1"),
    event("phase_event", 2, { stage: "model_process_summary", message: "same" }, "turn_2"),
  ]))).agentRuns[0];

  assert.deepEqual(projected.messages.map((message) => [message.turnId, message.text]), [
    ["turn_1", "same"],
    ["turn_2", "same"],
  ]);
  assert.deepEqual(projected.overlayBarrierByTurnId, { turn_1: 1, turn_2: 2 });
});

test("replayed committed phase is idempotent and keeps its overlay barrier", () => {
  const store = createChatViewStore();
  store.replaceAll([validateHistoryPage(page(historyAgentRun([]))).agentRuns[0]]);
  const phase = event("phase_event", 1, { stage: "model_process_summary", message: "once" }, "turn_2");
  store.applyStreamEntries([committedEntry(phase, "1-0")]);
  store.applyStreamEntries([committedEntry(phase, "2-0")]);

  const projected = store.getAgentRunSnapshot(agentRunId);
  assert.equal(projected.streamCursor, "2-0");
  assert.equal(projected.overlayBarrierByTurnId.turn_2, 1);
  assert.deepEqual(projected.messages.map((message) => message.text), ["once"]);
});

test("final assistant commit also rejects an older late live overlay for its turn", () => {
  const store = createChatViewStore();
  store.replaceAll([validateHistoryPage(page(historyAgentRun([
    event("agent_run_started", 1, { userObjective: "inspect" }),
    event("user_message", 2, { messageId: "message:user", text: "inspect", attachments: [] }),
  ]))).agentRuns[0]]);
  store.applyStreamEntries([committedEntry(event("assistant_message", 3, {
    messageId: "message:turn_2:assistant",
    modelMarkdown: "final",
    artifactRefs: [],
    status: "done",
  }, "turn_2"))]);
  store.applyStreamEntries([liveEntry(2, "late final", "4-0", { afterSequence: 2 })]);

  const projected = store.getAgentRunSnapshot(agentRunId);
  assert.equal(projected.live, null);
  assert.equal(projected.overlayBarrierByTurnId.turn_2, 3);
  assert.equal(projected.messages.at(-1).text, "final");
});

test("phase timing restores from events, resets only when the displayed phase changes, and terminal status always wins", () => {
  const stored = [
    event("agent_run_started", 1, { userObjective: "inspect" }),
    event("user_message", 2, { messageId: "message:user", text: "inspect", attachments: [] }),
    event("tool_call", 3, { callId: "read_1", toolName: "read", toolContractDigest: `sha256:${"a".repeat(64)}`, providerId: "centaeris.builtin", normalizedInput: { path: "a" }, displayTarget: "a" }),
    event("tool_result", 4, { callId: "read_1", toolName: "read", resultState: "successNoOutput", modelContent: "", summary: "read", latencyMs: 1, operations: [] }),
    event("phase_event", 5, { stage: "model_process_summary", message: "still thinking" }),
  ];
  [1_000, 2_000, 5_000, 7_000, 9_000].forEach((createdAtMs, index) => { stored[index].event.createdAtMs = createdAtMs; });
  const agentRun = validateHistoryPage(page(historyAgentRun(stored))).agentRuns[0];
  assert.equal(agentRun.phaseKey, "thinking");
  assert.equal(agentRun.phaseStartedAtMs, 7_000);

  let clock = 13_000;
  const store = createChatViewStore({ now: () => clock });
  store.replaceAll([agentRun]);
  const nextTool = event("tool_call", 6, { callId: "bash_1", toolName: "bash", toolContractDigest: `sha256:${"b".repeat(64)}`, providerId: "centaeris.builtin", normalizedInput: { command: "pwd" }, displayTarget: "pwd" });
  nextTool.event.createdAtMs = 12_000;
  store.applyStreamEntries([{ cursor: "6-0", item: { schema: "session.stream.item.v1", kind: "committed", agentRunId, sourceSequence: 6, event: nextTool.event } }]);
  assert.equal(store.getAgentRunSnapshot(agentRunId).phaseStartedAtMs, 12_000);
  store.updateConnection(agentRunId, "reconnecting");
  assert.equal(store.getAgentRunSnapshot(agentRunId).phaseStartedAtMs, 13_000);
  clock = 14_000;
  store.updateConnection(agentRunId, "reconnecting");
  assert.equal(store.getAgentRunSnapshot(agentRunId).phaseStartedAtMs, 13_000);
  clock = 15_000;
  store.updateConnection(agentRunId, "running");
  assert.equal(store.getAgentRunSnapshot(agentRunId).phaseStartedAtMs, 15_000);
  store.applyStreamEntries([liveEntry(1, "still running", "7-0")]);
  assert.equal(store.getAgentRunSnapshot(agentRunId).phaseStartedAtMs, 15_000);

  for (const status of ["completed", "failed", "cancelled", "interrupted"]) {
    assert.equal(isAgentRunActive({ status, connection: "reconnecting" }), false);
  }
  assert.equal(isAgentRunActive({ status: "queued", connection: "starting" }), true);
  assert.equal(isAgentRunActive({ status: "running", connection: "reconnecting" }), true);
});

test("phase elapsed text uses a fake wall clock and interruption creates no transcript marker", () => {
  assert.equal(formatPhaseElapsed(1_000, 1_000), "");
  assert.equal(formatPhaseElapsed(1_000, 1_999), "");
  assert.equal(formatPhaseElapsed(1_000, 9_000), "8s");
  assert.equal(formatPhaseElapsed(1_000, 129_000), "2m 08s");
  assert.equal(formatPhaseElapsed(1_000, 3_729_000), "1h 02m 08s");
  assert.equal(formatPhaseElapsed(1_000, 97_205_000), "27h 00m 04s");
  assert.equal(formatPhaseElapsed(2_000, 1_000), "");
  assert.equal(formatPhaseElapsed(undefined, 1_000), "");

  const interrupted = validateHistoryPage(page(historyAgentRun([
    event("agent_run_started", 1, { userObjective: "inspect" }),
    event("user_message", 2, { messageId: "message:user", text: "inspect", attachments: [] }),
    event("agent_run_interrupted", 3, { reasonType: "cancelled", message: "stopped", retryable: false }),
  ], { status: "cancelled", completedAt: "2026-08-15T00:00:01Z" }))).agentRuns[0];
  assert.equal(interrupted.status, "cancelled");
  assert.equal(Object.hasOwn(interrupted, "cancellationMessage"), false);
});

test("stream render telemetry keeps the last coalesced entry and waits for a paint opportunity", () => {
  let clock = 1;
  let nextFrameId = 0;
  const reducerFrames = [];
  const paintFrames = [];
  const cancelledReducerFrames = [];
  const cancelledPaintFrames = [];
  const metrics = [];
  const schedule = (frames) => (callback) => {
    nextFrameId += 1;
    frames.push({ callback, frameId: nextFrameId });
    return nextFrameId;
  };
  const store = createChatViewStore({
    monotonicNow: () => clock,
    scheduleFrame: schedule(paintFrames),
    cancelFrame: (frameId) => cancelledPaintFrames.push(frameId),
    onRenderTelemetry: (metric) => metrics.push(metric),
  });
  store.replaceAll([validateHistoryPage(page(historyAgentRun([]))).agentRuns[0]]);
  const controller = new WorkspaceChatController({
    store,
    workspaceId: "workspace_1",
    sessionId,
    agentRunId,
    scheduleFrame: schedule(reducerFrames),
    cancelFrame: (frameId) => cancelledReducerFrames.push(frameId),
    now: () => clock,
  });

  controller.accept(liveEntry(1, "旧正文"));
  clock = 2;
  controller.accept(liveEntry(2, "最后正文"));
  assert.equal(controller.queuedItemCount(), 1);

  clock = 3;
  reducerFrames.shift().callback();
  const projected = store.getAgentRunSnapshot(agentRunId);
  assert.equal(projected.messages.at(-1).text, "最后正文");

  clock = 4;
  const cleanup = store.markDomCommit(agentRunId, store.getStreamRenderRevision(agentRunId));
  assert.equal(metrics.length, 0);
  clock = 5;
  paintFrames.shift().callback();
  assert.equal(metrics.length, 0, "the first frame only creates a paint opportunity");
  clock = 6;
  paintFrames.shift().callback();

  assert.deepEqual(metrics, [{
    schema: "workspace.chat_render_telemetry.v1",
    agentRunId,
    acceptedOrdinal: 2,
    streamItemKind: "live",
    acceptedAt: 2,
    reducerAppliedAt: 3,
    domCommitAt: 4,
    domPaintBoundaryAt: 6,
    acceptedToReducerMs: 1,
    reducerToDomCommitMs: 1,
    domCommitToPaintBoundaryMs: 2,
    acceptedToDomPaintBoundaryMs: 4,
  }]);
  cleanup();

  clock = 7;
  controller.accept(liveEntry(3, "提交后卸载"));
  clock = 8;
  reducerFrames.shift().callback();
  clock = 9;
  const cancelPendingPaint = store.markDomCommit(
    agentRunId,
    store.getStreamRenderRevision(agentRunId),
  );
  clock = 10;
  paintFrames.shift().callback();
  const scheduledPaintFrame = paintFrames.at(-1).frameId;
  cancelPendingPaint();
  assert.deepEqual(cancelledPaintFrames, [scheduledPaintFrame]);
  assert.equal(metrics.length, 1);

  clock = 11;
  controller.accept(liveEntry(4, "不会应用"));
  const scheduledReducerFrame = reducerFrames.at(-1).frameId;
  controller.dispose();
  assert.deepEqual(cancelledReducerFrames, [scheduledReducerFrame]);
});

test("live SSE without an id does not advance the durable cursor", () => {
  const store = createChatViewStore();
  store.replaceAll([validateHistoryPage(page(historyAgentRun([]))).agentRuns[0]]);
  const controller = new WorkspaceChatController({
    store,
    workspaceId: "workspace_1",
    sessionId,
    agentRunId,
    initialCursor: "v1.durable",
    scheduleFrame: () => 1,
    cancelFrame: () => {},
  });

  controller.accept({ ...liveEntry(1, "正文"), cursor: null });

  assert.equal(controller.lastCursor, "v1.durable");
});

test("controller exposes only the last successfully applied durable cursor", async () => {
  const store = createChatViewStore();
  store.replaceAll([validateHistoryPage(page(historyAgentRun([]))).agentRuns[0]]);
  const frames = [];
  const controller = new WorkspaceChatController({
    store,
    workspaceId: "workspace_1",
    sessionId,
    agentRunId,
    initialCursor: "1-0",
    scheduleFrame: (callback) => (frames.push(callback), frames.length),
    cancelFrame: () => {},
  });

  controller.accept(committedEntry(event("agent_run_started", 1, { userObjective: "cursor" }), "2-0"));
  assert.equal(controller.receivedCursor, "2-0");
  assert.equal(controller.lastCursor, "1-0", "reconnect must stay on the applied cursor while an entry is queued");

  frames.shift()();
  await controller.whenIdle();
  assert.equal(controller.lastCursor, "2-0");
});

test("controller keeps the applied cursor when an atomic reducer batch fails", async () => {
  const baseStore = createChatViewStore();
  baseStore.replaceAll([validateHistoryPage(page(historyAgentRun([]))).agentRuns[0]]);
  const store = {
    ...baseStore,
    applyStreamEntries() {
      throw new Error("projection failed");
    },
  };
  const frames = [];
  const controller = new WorkspaceChatController({
    store,
    workspaceId: "workspace_1",
    sessionId,
    agentRunId,
    initialCursor: "1-0",
    scheduleFrame: (callback) => (frames.push(callback), frames.length),
    cancelFrame: () => {},
  });

  controller.accept(committedEntry(event("agent_run_started", 1, { userObjective: "cursor" }), "2-0"));
  frames.shift()();

  await assert.rejects(controller.whenIdle(), /projection failed/);
  assert.equal(controller.receivedCursor, "2-0");
  assert.equal(controller.lastCursor, "1-0");
});

test("history schema and global event order loud-fail", () => {
  assert.throws(() => validateHistoryPage({ ...page(historyAgentRun([])), schema: "banana" }), /invalid session history page/);
  const projected = validateHistoryPage(page(historyAgentRun([
    event("agent_run_started", 2, { userObjective: "x" }),
    event("user_message", 1, { messageId: "message:user", text: "x", attachments: [] }),
  ]))).agentRuns[0];
  assert.equal(projected.projectionError, "session_projection_invalid");
});

test("store marks only the selected AgentRun as unprojectable", () => {
  const store = createChatViewStore();
  const first = validateHistoryPage(page(historyAgentRun([]))).agentRuns[0];
  const second = { ...first, id: "agent_run_2", projectionError: null };
  store.replaceAll([first, second]);

  store.markAgentRunProjectionError(first.id);

  assert.equal(store.getAgentRunSnapshot(first.id).projectionError, "session_projection_invalid");
  assert.equal(store.getAgentRunSnapshot(second.id).projectionError, null);
});
