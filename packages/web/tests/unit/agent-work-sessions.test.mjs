import assert from "node:assert/strict";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { ApiError, button, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";
const { AgentChatPageContent } = await import(await subjectLoader()(fileURLToPath(new URL("../../src/routes/AgentChatRoute.tsx", import.meta.url))));
const history = { schema: "agent.history.v1", agentId: "agent", sessionId: "coord", items: [], nextCursor: "tail", newestCursor: "tail", hasMore: false };
const session = (id = "unreferenced-work") => ({ id, workspaceId: "workspace", agentId: "agent", projectId: null, title: `Work ${id}`, origin: "automation", status: "active", deletedAt: null, isPinned: false, isUnread: false, hasActiveAgentRun: false, updatedAt: "2026-10-05T00:00:00Z", initialInputOrigin: { messageId: `initial-${id}`, agentId: "agent", agentName: "Synthetic Agent" } });
const page = (sessions = [session()], hasMore = false) => ({ schema: "agent.work_sessions.v1", agentId: "agent", workspaceId: "workspace", sessions, nextAfterSessionId: hasMore ? sessions.at(-1).id : null, hasMore });
const { useAgentWorkSessions } = await import(await subjectLoader()(fileURLToPath(new URL("../../src/agent-chat/useAgentWorkSessions.ts", import.meta.url))));

test("background work-list polling preserves loaded rows and does not become visible loading", async context => {
  const timers = new Map(); let id = 0; let release; let calls = 0;
  context.mock.method(globalThis, "setTimeout", (fn, delay) => { timers.set(++id, { fn, delay }); return id; });
  context.mock.method(globalThis, "clearTimeout", value => timers.delete(value));
  function WorkList() { return useAgentWorkSessions("agent", "workspace", true, () => {}); }
  const subject = renderer(WorkList, {}, environment({ request: async () => ++calls === 1 ? page() : new Promise(resolve => { release = resolve; }) }));
  try {
    await subject.settle();
    const before = subject.tree.sessions;
    assert.equal(subject.tree.loading, false);
    [...timers.values()].find(timer => timer.delay === 5000).fn();
    await subject.settle();
    assert.equal(subject.tree.busy, true);
    assert.equal(subject.tree.loading, false);
    assert.equal(subject.tree.sessions, before);
    release(page()); await subject.settle();
    assert.equal(subject.tree.busy, false);
    assert.equal(subject.tree.revision, 2);
  } finally { subject.unmount(); }
});
const component = (tree, name) => nodes(tree, n => n.type.name === name)[0];
const { agentWorkSessionsPath, parseAgentWorkSessionsPage, parseSessionInputOrigin } = await import(await subjectLoader()(fileURLToPath(new URL("../../src/agent-chat/agentWorkSessions.ts", import.meta.url))));

test("work pages preserve authoritative nonlexical order and reject malformed scope, rows and cursors", () => {
  const value = page([session("work-z"), session("work-99"), session("work-100")], true);
  assert.deepEqual(parseAgentWorkSessionsPage(value, "agent", "workspace", null).sessions.map(s => s.sessionId), ["work-z", "work-99", "work-100"]);
  for (const malformed of [
    { ...value, agentId: "other" }, { ...value, workspaceId: "foreign" }, { ...value, unknown: true },
    { ...value, nextAfterSessionId: "work-z" }, { ...value, sessions: [session("same"), session("same")] },
    { ...value, sessions: [{ ...session(), initialInputOrigin: undefined }] },
    { ...value, sessions: [{ ...session(), initialInputOrigin: { messageId: "message", agentId: "agent", agentName: "Name", unknown: true } }] },
  ]) assert.throws(() => parseAgentWorkSessionsPage(malformed, "agent", "workspace", null));
  assert.throws(() => parseAgentWorkSessionsPage(value, "agent", "workspace", "work-z"));
  assert.throws(() => parseAgentWorkSessionsPage(value, "agent", "workspace", null, 2));
  const request = new URL(agentWorkSessionsPath("agent/a", "cursor/?+="), "https://fixture.invalid");
  assert.equal(request.pathname, "/api/agents/agent%2Fa/work-sessions"); assert.equal(request.searchParams.get("afterSessionId"), "cursor/?+=");
  for (const limit of [0, 101, 1.5]) assert.throws(() => agentWorkSessionsPath("agent", null, limit));
  assert.equal(parseSessionInputOrigin({ session: { ...session(), initialInputOrigin: null } }, "workspace", "agent", "unreferenced-work"), null);
  assert.throws(() => parseSessionInputOrigin({ session: session() }, "workspace", "other", "unreferenced-work"));
});

test("a work listed independently hydrates later reply references once and retains its authoritative running state", async context => {
  const timers = new Map(); let timerId = 0; let arrival = false; const requests = [];
  context.mock.method(globalThis, "setTimeout", (fn, delay) => { timers.set(++timerId, { fn, delay }); return timerId; });
  context.mock.method(globalThis, "clearTimeout", id => timers.delete(id));
  const reply = (id, sequence) => ({ kind: "message", cursor: `reply-${id}`, message: { id, agentRunId: "run", turnId: "turn", toolCallId: id, sourceSequence: sequence, createdAtMs: sequence, body: id, sessionRefs: ["unreferenced-work"], fileRefs: [] } });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, environment({ request: async path => {
    requests.push(path);
    if (path.includes("/work-sessions")) return page();
    if (path.startsWith("/api/sessions/")) throw new Error("listed work must not be hydrated again");
    return arrival ? { ...history, items: [reply("first", 1), reply("second", 2)], nextCursor: "arrived", newestCursor: "arrived" } : history;
  } }));
  try {
    await subject.settle(); button(subject.tree, "agentChat.toggleOverview").props.onClick(); await subject.settle();
    component(subject.tree, "AgentOverviewPanel").props.onRunState("unreferenced-work", "running"); await subject.settle();
    component(subject.tree, "AgentOverviewPanel").props.onPreviewSession("unreferenced-work"); await subject.settle();
    assert.equal(component(subject.tree, "AgentSessionPreviewShell").props.session.runState, "running");
    arrival = true; for (const timer of [...timers.values()]) if (timer.delay === 5000) timer.fn(); await subject.settle();
    const messages = component(subject.tree, "AgentMessageList").props.messages;
    assert.deepEqual(messages.map(m => m.sessions.map(s => s.sessionId)), [["unreferenced-work"], ["unreferenced-work"]], "each reply keeps only its own deliberate references");
    assert.equal(requests.filter(p => p.startsWith("/api/sessions/")).length, 0);
    assert.equal(component(subject.tree, "AgentSessionPreviewShell").props.session.sessionId, "unreferenced-work");
  } finally { subject.unmount(); }
});

test("overview independently reads admitted work without reply references and keeps ordinary Session navigation", async () => {
  const requests = [];
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, environment({ request: async path => { requests.push(path); return path.includes("/work-sessions") ? page() : history; } }));
  try {
    await subject.settle();
    assert.equal(requests.filter(p => p.includes("/work-sessions")).length, 0, "closed overview performs no work-list reads");
    button(subject.tree, "agentChat.toggleOverview").props.onClick(); await subject.settle();
    const overview = component(subject.tree, "AgentOverviewPanel");
    assert.deepEqual(overview.props.sessions.map(s => s.sessionId), ["unreferenced-work"]);
    assert.equal(requests.filter(p => p.includes("/work-sessions")).length, 1);
    overview.props.onPreviewSession("unreferenced-work"); await subject.settle();
    assert.equal(component(subject.tree, "AgentSessionPreviewShell").props.session.sessionId, "unreferenced-work");
  } finally { subject.unmount(); }
});

test("one explicit page action reads one page; overview refresh revalidates only the previously opened page window", async context => {
  const timers = new Map(); let timerId = 0; const requests = [];
  context.mock.method(globalThis, "setTimeout", (fn, delay) => { timers.set(++timerId, { fn, delay }); return timerId; });
  context.mock.method(globalThis, "clearTimeout", id => timers.delete(id));
  const first = Array.from({ length: 50 }, (_, i) => session(`work-${i}`));
  const second = Array.from({ length: 50 }, (_, i) => session(`work-${i + 50}`));
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, environment({ request: async path => {
    if (!path.includes("/work-sessions")) return history;
    requests.push(path); return path.includes("afterSessionId=") ? page(second, true) : page(first, true);
  } }));
  try {
    await subject.settle(); button(subject.tree, "agentChat.toggleOverview").props.onClick(); await subject.settle();
    assert.equal(requests.length, 1, "hasMore never causes an automatic full traversal");
    component(subject.tree, "AgentOverviewPanel").props.pagination.onLoadMore(); await subject.settle();
    assert.equal(requests.length, 2);
    assert.match(requests[1], /afterSessionId=work-49/);
    assert.equal(component(subject.tree, "AgentOverviewPanel").props.sessions.length, 100);
    for (const timer of [...timers.values()]) if (timer.delay === 5000) timer.fn();
    await subject.settle();
    assert.equal(requests.length, 4, "refresh is bounded by two explicitly opened pages even when another page exists");
    button(subject.tree, "agentChat.toggleOverview").props.onClick(); await subject.settle();
    const count = requests.length;
    for (const timer of [...timers.values()]) if (timer.delay === 5000) timer.fn();
    await subject.settle(); assert.equal(requests.length, count, "closed overview cancels its poll");
  } finally { subject.unmount(); }
});

test("work-list authorization rejection removes old metadata instead of exposing a cached work", async context => {
  const timers = new Map(); let timerId = 0; let denied = false;
  context.mock.method(globalThis, "setTimeout", (fn, delay) => { timers.set(++timerId, { fn, delay }); return timerId; });
  context.mock.method(globalThis, "clearTimeout", id => timers.delete(id));
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, environment({ request: async path => {
    if (!path.includes("/work-sessions")) return history;
    if (denied) throw new ApiError("agent_not_found", 404);
    return page();
  } }));
  try {
    await subject.settle(); button(subject.tree, "agentChat.toggleOverview").props.onClick(); await subject.settle();
    assert.equal(component(subject.tree, "AgentOverviewPanel").props.sessions.length, 1);
    denied = true;
    for (const timer of [...timers.values()]) if (timer.delay === 5000) timer.fn();
    await subject.settle();
    assert.equal(component(subject.tree, "AgentOverviewPanel"), undefined);
    assert.equal(button(subject.tree, "agentChat.toggleOverview").props["aria-expanded"], false);
  } finally { subject.unmount(); }
});
