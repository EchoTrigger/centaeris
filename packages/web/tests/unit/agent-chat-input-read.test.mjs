import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createJsxSubjectLoader } from "./jsxSubject.mjs";
import { ApiError, environment, nodes, renderer, subjectLoader, tailRefreshFixture } from "./componentHarness.mjs";

const { AgentChatPageContent } = await import(await subjectLoader()(fileURLToPath(new URL("../../src/routes/AgentChatRoute.tsx", import.meta.url))));
const stub = code => `data:text/javascript;base64,${Buffer.from(code).toString("base64")}`;
const bubbleLoader = createJsxSubjectLoader(new Map([["../chat/MarkdownContent", stub("export const MarkdownContent=()=>null;")], ["./AgentSessionReference", stub("export const AgentSessionReference=()=>null;")], ["./AgentPreviewContent", stub("export const AgentMessageFiles=()=>null;")]]));
const { AgentMessageBubble } = await import(await bubbleLoader(fileURLToPath(new URL("../../src/agent-chat/AgentMessageBubble.tsx", import.meta.url))));
const input = { inputId: "input-one", sequence: 3, createdAtMs: 2, body: "  exact  ", attachments: [], read: null };
const read = { agentRunId: "run", eventId: "event", requestId: "request", createdAtMs: 4 };
const row = { kind: "input", cursor: "opaque-input-position", input };
const page = items => ({ schema: "agent.history.v1", agentId: "agent", sessionId: "coord", items, nextCursor: "full-tail", newestCursor: "full-tail", hasMore: false });
const messages = tree => nodes(tree, node => node.type.name === "AgentMessageList")[0].props.messages;

test("actual message bubble shows canonical Read only on the latest user with matching input identity", () => {
  const user = { role: "user", messageId: input.inputId, authorLabel: "You", text: input.body, isLatest: true,
    loopInputFact: { kind: "loopInput", messageId: input.inputId, receivedAt: "1970-01-01T00:00:00.004Z" } };
  const times = message => nodes(AgentMessageBubble({ message, readLabel: "Read", locale: "en-US", timeZone: "UTC", onPreviewSession() {} }), node => node.type === "time");
  assert.equal(times(user)[0].props.dateTime, user.loopInputFact.receivedAt);
  assert.equal(times({ ...user, isLatest: false }).length, 0);
  assert.equal(times({ ...user, loopInputFact: { ...user.loopInputFact, messageId: "other" } }).length, 0);
  assert.equal(times({ ...user, role: "agent" }).length, 0);
});

test("the canonical user Read is outside the message bubble and follows its body", () => {
  const message = { role: "user", messageId: input.inputId, authorLabel: "You", text: input.body, isLatest: true,
    loopInputFact: { kind: "loopInput", messageId: input.inputId, receivedAt: "2026-10-04T13:45:00.000Z" } };
  const tree = AgentMessageBubble({ message, readLabel: "已读", locale: "zh-CN", timeZone: "UTC", onPreviewSession() {} });
  const article = nodes(tree, node => node.type === "article")[0];
  const time = nodes(tree, node => node.type === "time")[0];
  assert.equal(nodes(article, node => node === time).length, 0, "Read must be outside the bordered message article");
  assert.equal(nodes(article, node => node.props.className === "agentChatUserText")[0].props.children, input.body);
  assert.equal(time.props.dateTime, message.loopInputFact.receivedAt);
  const footer = nodes(tree, node => node.type === "footer")[0];
  const children = tree.props.children.flat().filter(Boolean);
  assert.ok(children.indexOf(footer) > children.indexOf(article), "Read follows the user's message, instead of entering the reply body");
  assert.equal(nodes(footer, node => node.type === "span")[1].props.children[0], "已读");
});

test("moving Read preserves unconsumed inputs, mismatched facts and content-free reference presentation", () => {
  const user = { role: "user", messageId: input.inputId, authorLabel: "You", text: "", isLatest: true };
  const render = message => AgentMessageBubble({ message, readLabel: "Read", locale: "en-US", timeZone: "UTC", onPreviewSession() {} });
  assert.equal(nodes(render(user), node => node.type === "footer").length, 0, "Pending and unread inputs have no uptake fact");
  const matching = { kind: "loopInput", messageId: input.inputId, receivedAt: "2026-10-04T13:45:00.000Z" };
  assert.equal(nodes(render({ ...user, loopInputFact: matching }), node => node.type === "time").length, 1, "Read does not depend on visible body text");
  assert.equal(nodes(render({ ...user, loopInputFact: { ...matching, messageId: "other" } }), node => node.type === "footer").length, 0);
  assert.equal(nodes(render({ ...user, loopInputFact: { ...matching, receivedAt: "invalid" } }), node => node.type === "footer").length, 0);
  const sessionOnly = render({ role: "agent", messageId: "reference-only", authorLabel: "Agent", text: "", isLatest: true,
    sessions: [{ sessionId: "work", title: "Session without reply text", runState: "unknown" }] });
  const article = nodes(sessionOnly, node => node.type === "article")[0];
  const reference = nodes(sessionOnly, node => node.type.name === "AgentSessionReference")[0];
  assert.equal(reference.props.session.sessionId, "work");
  assert.equal(nodes(article, node => node === reference).length, 0);
  assert.equal(nodes(sessionOnly, node => node.type === "footer").length, 0, "Agent references do not manufacture user Read");
});

test("Agent chat exposes the real input composer after the reviewed history", async () => {
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, environment({ request: async () => page([]) }));
  await subject.settle();
  assert.equal(nodes(subject.tree, node => node.type.name === "AgentInputComposer").length, 1);
  assert.equal(nodes(subject.tree, node => node.props.children === "agentChat.inputUnavailable").length, 0);
  subject.unmount();
});

test("Read endpoint revocation clears previously authorized history", async context => {
  const refresh = tailRefreshFixture(context);
  let revoked = false; let historyGets = 0;
  const env = environment({ request: async path => {
    if (path.includes("/history")) return page(++historyGets === 1 ? [row] : []);
    if (revoked) throw new ApiError("coordination_session_not_found", 404);
    return { schema: "agent.inputs.v1", agentId: "agent", sessionId: "coord", inputs: [input], nextAfterSequence: null };
  } });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env); await subject.settle();
  revoked = true; await refresh(subject);
  assert.deepEqual(messages(subject.tree), []); subject.unmount();
});

test("automatic tail refresh is read-only, bounded and stops on unmount", async () => {
  const originalSet = globalThis.setTimeout; const originalClear = globalThis.clearTimeout;
  const timers = new Map(); let index = 0; const gets = [];
  globalThis.setTimeout = (callback, delay) => { assert.equal(delay, 5000); timers.set(++index, callback); return index; };
  globalThis.clearTimeout = id => timers.delete(id);
  let subject;
  try {
    subject = renderer(AgentChatPageContent, { agentId: "agent" }, environment({ request: async (path, options) => {
      assert.equal(options?.method || "GET", "GET"); gets.push(path);
      return path.includes("/history") ? page(gets.length === 1 ? [row] : []) : { schema: "agent.inputs.v1", agentId: "agent", sessionId: "coord", inputs: [input], nextAfterSequence: null };
    } }));
    await subject.settle(); assert.equal(timers.size, 1);
    [...timers.values()][0](); await subject.settle();
    assert.equal(gets.filter(path => path.includes("/history")).length, 2);
    assert.equal(gets.filter(path => path.includes("/inputs?")).length, 2);
    subject.unmount(); assert.equal(timers.size, 0);
  } finally { subject?.unmount(); globalThis.setTimeout = originalSet; globalThis.clearTimeout = originalClear; }
});

test("empty history tail refreshes the already loaded latest input Read without moving it", async context => {
  const refresh = tailRefreshFixture(context);
  let historyGets = 0; let consumed = false; const gets = [];
  const env = environment({ request: async path => {
    gets.push(path);
    if (path.includes("/history")) return page(++historyGets === 1 ? [row] : []);
    assert.equal(new URL(path, "https://fixture.invalid").searchParams.get("afterSequence"), "2");
    assert.equal(new URL(path, "https://fixture.invalid").searchParams.get("limit"), "1");
    return { schema: "agent.inputs.v1", agentId: "agent", sessionId: "coord", inputs: [{ ...input, read: consumed ? read : null }], nextAfterSequence: null };
  } });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  await subject.settle();
  assert.equal(messages(subject.tree)[0].loopInputFact, undefined);
  consumed = true; await refresh(subject);
  assert.deepEqual(messages(subject.tree).map(message => message.messageId), ["input-one"]);
  assert.deepEqual(messages(subject.tree)[0].loopInputFact, { kind: "loopInput", messageId: "input-one", receivedAt: "1970-01-01T00:00:00.004Z" });
  assert.equal(new URL(gets.find(path => path.includes("afterCursor")), "https://fixture.invalid").searchParams.get("afterCursor"), "full-tail");
  subject.unmount();
});
