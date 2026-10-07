import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { ApiError, button, deferred, environment, nodes, renderer, subjectLoader, tailRefreshFixture } from "./componentHarness.mjs";

const { AgentChatPageContent } = await import(await subjectLoader()(fileURLToPath(new URL("../../src/routes/AgentChatRoute.tsx", import.meta.url))));
const row = { kind: "message", cursor: "message-position", message: { id: "reply", agentRunId: "run", turnId: "turn", toolCallId: "call", sourceSequence: 1, createdAtMs: 1, body: "Synthetic committed reply", sessionRefs: ["work"], fileRefs: [] } };
const page = (items = [row], nextCursor = "opaque-full-tail") => ({ schema: "agent.history.v1", agentId: "agent", sessionId: "coordination", items, nextCursor, newestCursor: nextCursor, hasMore: false });
const session = title => ({ session: { id: "work", workspaceId: "workspace", agentId: "worker", title, status: "active", hasActiveAgentRun: false } });
const messages = tree => nodes(tree, node => node.type.name === "AgentMessageList")[0].props;
const preview = tree => nodes(tree, node => node.type.name === "AgentSessionPreviewShell")[0]?.props;
const replies = tree => nodes(tree, node => node.type === "section" && node.props.className === "agentChatRouteReplies")[0];
const historyRow = (name, sequence, sessionRefs = []) => ({ ...row, cursor: `cursor-${name}`, message: { ...row.message, id: name, sourceSequence: sequence, body: name, sessionRefs } });
const latestPage = () => ({ ...page([historyRow("latest", 2)], "older-boundary"), newestCursor: "latest-boundary", hasMore: true });
const olderPage = () => page([historyRow("older", 1, ["work"])], "oldest-boundary");
async function withScroll(subject, loadingHeight = 0, referenceHeight = 0) {
  let top = 800; let messageHeight = 1000;
  const root = { scrollHeight: 1000, clientHeight: 200, querySelector: () => ({ getBoundingClientRect: () => ({ height: messageHeight }) }), get scrollTop() { return top; }, set scrollTop(value) { top = Math.max(0, Math.min(value, this.scrollHeight - this.clientHeight)); } };
  const render = subject.render;
  subject.render = () => {
    const tree = render();
    const loading = nodes(tree, node => node.type === "p" && node.props.role === "status").length > 0;
    const hasReferences = messages(tree).messages.some(message => message.sessions?.length);
    messageHeight = (messages(tree).messages.length > 1 ? 1500 : 1000) + (hasReferences ? referenceHeight : 0);
    root.scrollHeight = messageHeight + (loading ? loadingHeight : 0);
    root.scrollTop = root.scrollTop;
    return tree;
  };
  subject.render(); replies(subject.tree).props.ref.current = root; subject.flush(); await subject.settle();
  return root;
}

const loadingStatuses = tree => nodes(tree, node => node.props.role === "status" && node.props.children === "agentChat.loading");

test("initial Agent history loads without a foreground loading message", async () => {
  const pending = deferred();
  const env = environment({ request: async () => pending.promise });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  try {
    await subject.settle();
    assert.deepEqual(messages(subject.tree).messages, []);
    assert.equal(loadingStatuses(subject.tree).length, 0);
    pending.resolve(page([historyRow("authorized reply", 2)], "latest-boundary")); await subject.settle();
    assert.deepEqual(messages(subject.tree).messages.map(message => message.text), ["authorized reply"]);
    assert.equal(loadingStatuses(subject.tree).length, 0);
  } finally { subject.unmount(); }
});

test("background tail refresh keeps history, preview and reading position without a loading message", async context => {
  const refresh = tailRefreshFixture(context); const pendingTail = deferred();
  const env = environment({ request: async path => {
    if (!path.startsWith("/api/agents/")) return session("Authorized title");
    return new URL(path, "https://fixture.invalid").searchParams.has("afterCursor") ? pendingTail.promise : page();
  } });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  try {
    const root = await withScroll(subject);
    messages(subject.tree).onPreviewSession("work"); await subject.settle(); root.scrollTop = 320;
    await refresh(subject);
    assert.deepEqual(messages(subject.tree).messages.map(message => message.text), [row.message.body]);
    assert.equal(preview(subject.tree).session.title, "Authorized title");
    assert.equal(root.scrollTop, 320);
    assert.equal(loadingStatuses(subject.tree).length, 0);
    pendingTail.resolve(page([], "opaque-full-tail")); await subject.settle();
    assert.equal(messages(subject.tree).messages[0].text, row.message.body);
    assert.equal(preview(subject.tree).session.title, "Authorized title");
    assert.equal(root.scrollTop, 320);
    assert.equal(loadingStatuses(subject.tree).length, 0);
  } finally { subject.unmount(); }
});

test("seamless older history keeps the latest page and scroll anchor without a loading message", async () => {
  const pendingOlder = deferred(); let olderRequests = 0;
  const env = environment({ request: async path => {
    if (!path.startsWith("/api/agents/")) return session("Older work title");
    if (new URL(path, "https://fixture.invalid").searchParams.has("beforeCursor")) { olderRequests++; return pendingOlder.promise; }
    return latestPage();
  } });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  try {
    const root = await withScroll(subject);
    root.scrollTop = 40; replies(subject.tree).props.onScroll(); await subject.settle();
    replies(subject.tree).props.onScroll(); await subject.settle();
    assert.equal(olderRequests, 1, "An in-flight older page must retain request exclusion");
    assert.deepEqual(messages(subject.tree).messages.map(message => message.text), ["latest"]);
    assert.equal(loadingStatuses(subject.tree).length, 0);
    root.scrollTop = 200; replies(subject.tree).props.onScroll(); pendingOlder.resolve(olderPage()); await subject.settle();
    assert.deepEqual(messages(subject.tree).messages.map(message => message.text), ["older", "latest"]);
    assert.equal(root.scrollTop, 700);
    assert.equal(loadingStatuses(subject.tree).length, 0);
  } finally { subject.unmount(); }
});

test("Agent identity switch hides old history and preview while the new history request is pending", async () => {
  const pending = deferred();
  const env = environment({ request: async path => path.includes("/other/history") ? pending.promise : path.startsWith("/api/agents/") ? page() : session("Original authorized title") });
  env.agents.push({ ...env.agents[0], id: "other", name: "Other Agent" });
  const props = { agentId: "agent" }; const subject = renderer(AgentChatPageContent, props, env);
  try {
    await subject.settle(); messages(subject.tree).onPreviewSession("work"); await subject.settle();
    props.agentId = "other"; await subject.settle();
    assert.deepEqual(messages(subject.tree).messages, []);
    assert.equal(preview(subject.tree), undefined);
    const composer = nodes(subject.tree, node => node.type.name === "AgentInputComposer")[0].props;
    assert.equal(composer.agentId, "other"); assert.equal(composer.sessionId, undefined);
    assert.equal(loadingStatuses(subject.tree).length, 0);
    pending.resolve({ ...page([historyRow("new authorized reply", 1)], "other-boundary"), agentId: "other", sessionId: "other-coordination" }); await subject.settle();
    assert.deepEqual(messages(subject.tree).messages.map(message => message.text), ["new authorized reply"]);
    assert.equal(preview(subject.tree), undefined);
  } finally { subject.unmount(); }
});

test("Agent settings has no duplicate page-header entry; composer retains the authoritative scope", async () => {
  const env = environment({ request: async path => path.startsWith("/api/agents/") ? page() : session("Authorized title") });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  await subject.settle();
  assert.equal(nodes(subject.tree, node => node.props.to?.includes("/settings/agents")).length, 0);
  const composer = nodes(subject.tree, node => node.type.name === "AgentInputComposer")[0].props;
  assert.equal(composer.agentId, "agent"); assert.equal(composer.workspaceId, "workspace");
  assert.deepEqual(env.navigations, []); subject.unmount();
});

test("first missing coordination remains empty and makes no session creation or reference request", async () => {
  const calls = [];
  const env = environment({ request: async (path, options) => { calls.push([path, options?.method || "GET"]); throw new ApiError("coordination_session_not_found", 404); } });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  await subject.settle();
  assert.deepEqual(messages(subject.tree).messages, []); assert.equal(preview(subject.tree), undefined);
  assert.equal(nodes(subject.tree, node => node.props.children === "agentChat.noHistory").length, 0);
  assert.equal(calls.length, 1); assert.equal(calls[0][1], "GET"); subject.unmount();
});

test("tail refresh revocation removes history, referenced titles and selected preview; retry hydrates anew", async context => {
  const refresh = tailRefreshFixture(context);
  let revoked = false; let title = "Original authorized title"; let referenceGets = 0;
  const env = environment({ request: async path => {
    if (path.startsWith("/api/agents/")) { if (revoked) throw new ApiError("coordination_session_not_found", 404); return page(); }
    referenceGets++; return session(title);
  } });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  await subject.settle(); messages(subject.tree).onPreviewSession("work"); await subject.settle();
  assert.equal(preview(subject.tree).session.title, title);
  revoked = true; await refresh(subject);
  assert.deepEqual(messages(subject.tree).messages, []);
  assert.equal(preview(subject.tree), undefined);
  assert.equal(button(subject.tree, "agentChat.retry")?.type, "button");
  revoked = false; title = "Newly authorized title";
  button(subject.tree, "agentChat.retry").props.onClick(); await subject.settle();
  assert.equal(referenceGets, 2);
  assert.equal(messages(subject.tree).messages[0].sessions[0].title, title);
  assert.equal(preview(subject.tree), undefined);
  subject.unmount();
});

test("transient history failure preserves visible history and preview for retry", async context => {
  const refresh = tailRefreshFixture(context);
  let failed = false;
  const env = environment({ request: async path => path.startsWith("/api/agents/") ? failed ? Promise.reject(new ApiError("temporary_unavailable", 503)) : page() : session("Authorized title") });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  await subject.settle(); messages(subject.tree).onPreviewSession("work"); await subject.settle();
  failed = true; await refresh(subject);
  assert.equal(messages(subject.tree).messages[0].text, row.message.body);
  assert.equal(preview(subject.tree).session.title, "Authorized title");
  assert.ok(button(subject.tree, "agentChat.retry")); subject.unmount();
});

test("empty tail refresh retries unresolved references without replaying or resetting the opaque history cursor", async context => {
  const refresh = tailRefreshFixture(context);
  let referenceGets = 0; const historyGets = [];
  const env = environment({ request: async path => {
    if (path.startsWith("/api/agents/")) { historyGets.push(path); return historyGets.length === 1 ? page() : page([], "opaque-full-tail"); }
    if (++referenceGets === 1) throw new ApiError("temporary_unavailable", 503);
    return session("Recovered title");
  } });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  await subject.settle();
  assert.ok(nodes(subject.tree, node => node.props.role === "alert" && node.props.children === "agentChat.referencesError").length);
  assert.deepEqual(messages(subject.tree).messages[0].sessions, []);
  await refresh(subject);
  assert.equal(referenceGets, 2);
  assert.equal(new URL(historyGets[1], "https://fixture.invalid").searchParams.get("afterCursor"), "opaque-full-tail");
  assert.equal(messages(subject.tree).messages.length, 1);
  assert.equal(messages(subject.tree).messages[0].sessions[0].title, "Recovered title");
  assert.equal(nodes(subject.tree, node => node.props.role === "alert").length, 0);
  subject.unmount();
});

test("failed history refresh keeps unresolved-reference error until those references actually retry", async context => {
  const refresh = tailRefreshFixture(context);
  let historyGets = 0;
  const env = environment({ request: async path => {
    if (path.startsWith("/api/agents/") && ++historyGets === 1) return page();
    throw new ApiError("temporary_unavailable", 503);
  } });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  await subject.settle(); await refresh(subject);
  assert.ok(nodes(subject.tree, node => node.props.role === "alert" && node.props.children === "agentChat.referencesError").length);
  assert.deepEqual(messages(subject.tree).messages[0].sessions, []); subject.unmount();
});

test("transient tail retry preserves loaded older messages, their preview and the reading position", async context => {
  const refresh = tailRefreshFixture(context);
  const calls = []; let failTail = true;
  const env = environment({ request: async path => {
    if (!path.startsWith("/api/agents/")) return session("Older work title");
    calls.push(path); const query = new URL(path, "https://fixture.invalid").searchParams;
    if (query.has("beforeCursor")) return olderPage();
    if (query.has("afterCursor")) { if (failTail) throw new ApiError("temporary_unavailable", 503); return page([], "latest-boundary"); }
    return latestPage();
  } });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  try {
    const root = await withScroll(subject);
    root.scrollTop = 40; replies(subject.tree).props.onScroll(); await subject.settle();
    messages(subject.tree).onPreviewSession("work"); await subject.settle(); root.scrollTop = 480;
    await refresh(subject); failTail = false;
    button(subject.tree, "agentChat.retry").props.onClick(); await subject.settle();
    assert.equal(calls.at(-1), calls.at(-2), "Retry must repeat the failed newest cursor");
    assert.deepEqual(messages(subject.tree).messages.map(message => message.text), ["older", "latest"]);
    assert.equal(preview(subject.tree).session.title, "Older work title");
    assert.equal(root.scrollTop, 480);
  } finally { subject.unmount(); }
});

test("transient older-page retry repeats its before cursor and prepends instead of replacing the latest page", async () => {
  const calls = []; let failOlder = true;
  const env = environment({ request: async path => {
    if (!path.startsWith("/api/agents/")) return session("Older work title");
    calls.push(path); const query = new URL(path, "https://fixture.invalid").searchParams;
    if (query.has("beforeCursor")) { if (failOlder) throw new ApiError("temporary_unavailable", 503); return olderPage(); }
    return latestPage();
  } });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  try {
    const root = await withScroll(subject);
    root.scrollTop = 40; replies(subject.tree).props.onScroll(); await subject.settle();
    failOlder = false; button(subject.tree, "agentChat.retry").props.onClick(); await subject.settle();
    assert.equal(calls.at(-1), calls.at(-2), "Retry must repeat the failed older cursor");
    assert.deepEqual(messages(subject.tree).messages.map(message => message.text), ["older", "latest"]);
    assert.equal(root.scrollTop, 540);
  } finally { subject.unmount(); }
});

test("revoked history retry starts from the latest page after clearing all old authority", async context => {
  const refresh = tailRefreshFixture(context);
  for (const status of [401, 403, 404]) {
    const calls = []; let revoked = false;
    const env = environment({ request: async path => {
      if (!path.startsWith("/api/agents/")) return session("Authorized title");
      calls.push(path); if (revoked) throw new ApiError("authority_lost", status); return page();
    } });
    const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
    try {
      await subject.settle(); messages(subject.tree).onPreviewSession("work"); await subject.settle();
      revoked = true; await refresh(subject);
      assert.deepEqual(messages(subject.tree).messages, []); assert.equal(preview(subject.tree), undefined);
      revoked = false; button(subject.tree, "agentChat.retry").props.onClick(); await subject.settle();
      assert.equal(calls.at(-1), calls[0], `${status}: cleared authority must reload the latest page`);
      assert.equal(messages(subject.tree).messages.length, 1); assert.equal(preview(subject.tree), undefined);
    } finally { subject.unmount(); }
  }
});

test("older prepend preserves scrolling during the request and does not replay its offset after reference hydration", async () => {
  const pendingOlder = deferred(); const pendingReference = deferred();
  const env = environment({ request: async path => {
    if (!path.startsWith("/api/agents/")) return pendingReference.promise;
    return new URL(path, "https://fixture.invalid").searchParams.has("beforeCursor") ? pendingOlder.promise : latestPage();
  } });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  try {
    const root = await withScroll(subject);
    root.scrollTop = 40; replies(subject.tree).props.onScroll(); await subject.settle();
    root.scrollTop = 200; replies(subject.tree).props.onScroll(); pendingOlder.resolve(olderPage()); await subject.settle();
    assert.deepEqual(messages(subject.tree).messages.map(message => message.text), ["older", "latest"]);
    assert.equal(root.scrollTop, 700, "Prepend must retain the position most recently chosen by the user");
    root.scrollTop = 900; pendingReference.resolve(session("Hydrated title")); await subject.settle();
    assert.equal(root.scrollTop, 900, "Finishing reference hydration must not restore an earlier position");
  } finally { subject.unmount(); }
});

test("an older Session reference adds only its new height to the position chosen during metadata loading", async () => {
  const pendingOlder = deferred(); const pendingReference = deferred();
  const env = environment({ request: async path => {
    if (!path.startsWith("/api/agents/")) return pendingReference.promise;
    return new URL(path, "https://fixture.invalid").searchParams.has("beforeCursor") ? pendingOlder.promise : latestPage();
  } });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  try {
    const root = await withScroll(subject, 41, 48);
    root.scrollTop = 40; replies(subject.tree).props.onScroll(); await subject.settle();
    root.scrollTop = 200; pendingOlder.resolve(olderPage()); await subject.settle();
    assert.equal(root.scrollTop, 700);
    root.scrollTop = 900; replies(subject.tree).props.onScroll();
    pendingReference.resolve(session("Hydrated title")); await subject.settle();
    assert.equal(root.scrollTop, 948, "The 48px attachment must preserve current reading, without replaying the earlier 700px offset");
  } finally { subject.unmount(); }
});

test("tail append does not follow the bottom after the user scrolls up during its request", async context => {
  const refresh = tailRefreshFixture(context); const pendingTail = deferred();
  const env = environment({ request: async path => new URL(path, "https://fixture.invalid").searchParams.has("afterCursor")
    ? pendingTail.promise : page([historyRow("latest", 2)], "latest-boundary") });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  try {
    const root = await withScroll(subject); root.scrollTop = 800;
    await refresh(subject); root.scrollTop = 300; replies(subject.tree).props.onScroll();
    pendingTail.resolve(page([historyRow("newer", 3)], "newer-boundary")); await subject.settle();
    assert.deepEqual(messages(subject.tree).messages.map(message => message.text), ["latest", "newer"]);
    assert.equal(root.scrollTop, 300);
  } finally { subject.unmount(); }
});

test("tail append keeps following when only the bottom loading indicator changes the scroll height", async context => {
  const refresh = tailRefreshFixture(context); const pendingTail = deferred();
  const env = environment({ request: async path => new URL(path, "https://fixture.invalid").searchParams.has("afterCursor")
    ? pendingTail.promise : page([historyRow("latest", 2)], "latest-boundary") });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  try {
    const root = await withScroll(subject, 48); root.scrollTop = root.scrollHeight - root.clientHeight;
    await refresh(subject);
    pendingTail.resolve(page([historyRow("newer", 3)], "newer-boundary")); await subject.settle();
    assert.equal(root.scrollTop, root.scrollHeight - root.clientHeight, "A loading indicator must not turn off following");
  } finally { subject.unmount(); }
});

test("scrolling after an older response but before its DOM commit updates the pending reading position", async () => {
  const pendingOlder = deferred(); const pendingReference = deferred(); let requestedReference = false;
  const env = environment({ request: async path => {
    if (!path.startsWith("/api/agents/")) { requestedReference = true; return pendingReference.promise; }
    return new URL(path, "https://fixture.invalid").searchParams.has("beforeCursor") ? pendingOlder.promise : latestPage();
  } });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  try {
    const root = await withScroll(subject);
    root.scrollTop = 40; replies(subject.tree).props.onScroll(); await subject.settle();
    root.scrollTop = 200; pendingOlder.resolve(olderPage());
    for (let attempt = 0; attempt < 20 && !requestedReference; attempt++) await Promise.resolve();
    assert.equal(requestedReference, true, "The response has been accepted before the pending React commit");
    assert.equal(messages(subject.tree).messages.length, 1);
    root.scrollTop = 350; replies(subject.tree).props.onScroll(); await subject.settle();
    assert.equal(root.scrollTop, 850, "A later scroll event must replace the pending snapshot before prepending");
    pendingReference.resolve(session("Hydrated title")); await subject.settle();
    assert.equal(root.scrollTop, 850);
  } finally { subject.unmount(); }
});

test("multiple small upward scrolls before a tail DOM commit cancel following cumulatively", async context => {
  const refresh = tailRefreshFixture(context); const pendingTail = deferred(); const pendingReference = deferred(); let requestedReference = false;
  const env = environment({ request: async path => {
    if (!path.startsWith("/api/agents/")) { requestedReference = true; return pendingReference.promise; }
    return new URL(path, "https://fixture.invalid").searchParams.has("afterCursor") ? pendingTail.promise : page([historyRow("latest", 2)], "latest-boundary");
  } });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  try {
    const root = await withScroll(subject); root.scrollTop = 800; await refresh(subject);
    pendingTail.resolve(page([historyRow("newer", 3, ["work"])], "newer-boundary"));
    for (let attempt = 0; attempt < 20 && !requestedReference; attempt++) await Promise.resolve();
    assert.equal(requestedReference, true);
    root.scrollTop = 780; replies(subject.tree).props.onScroll(); root.scrollTop = 760; replies(subject.tree).props.onScroll(); await subject.settle();
    assert.equal(root.scrollTop, 760, "Several small movements must count as the user's total move away from the bottom");
    pendingReference.resolve(session("Hydrated title")); await subject.settle(); assert.equal(root.scrollTop, 760);
  } finally { subject.unmount(); }
});

test("initial following reaches the final bottom when a 41px loading line becomes a 44px hydrated reference", async () => {
  const pendingReference = deferred();
  const env = environment({ request: async path => path.startsWith("/api/agents/") ? page() : pendingReference.promise });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  try {
    const root = await withScroll(subject, 41, 44);
    assert.equal(root.scrollTop, root.scrollHeight - root.clientHeight);
    pendingReference.resolve(session("Hydrated title")); await subject.settle();
    assert.equal(root.scrollTop, root.scrollHeight - root.clientHeight, "The final 3px increase must remain followed");
  } finally { subject.unmount(); }
});

test("initial reference hydration does not return a user who moved up even slightly to the bottom", async () => {
  for (const [movedUp, notifiedScroll] of [[1, false], [1, true], [120, true]]) {
    const pendingReference = deferred();
    const env = environment({ request: async path => path.startsWith("/api/agents/") ? page() : pendingReference.promise });
    const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
    try {
      const root = await withScroll(subject, 41, 44); root.scrollTop -= movedUp;
      const readingPosition = root.scrollTop; if (notifiedScroll) replies(subject.tree).props.onScroll();
      pendingReference.resolve(session("Hydrated title")); await subject.settle();
      assert.equal(root.scrollTop, readingPosition, `${movedUp}px of upward reading must cancel final following`);
    } finally { subject.unmount(); }
  }
});

test("a message-only older page batched with loading completion excludes the removed 45px loading line", async () => {
  const pendingOlder = deferred();
  const env = environment({ request: async path => new URL(path, "https://fixture.invalid").searchParams.has("beforeCursor") ? pendingOlder.promise : latestPage() });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  try {
    const root = await withScroll(subject, 45);
    root.scrollTop = 40; replies(subject.tree).props.onScroll(); await subject.settle();
    root.scrollTop = 200; replies(subject.tree).props.onScroll();
    pendingOlder.resolve(page([historyRow("older", 1)], "oldest-boundary"));
    // There are no inputs or references to fetch. Let the request finish so React
    // can commit its items and removed loading indicator together.
    for (let attempt = 0; attempt < 20; attempt++) await Promise.resolve();
    assert.equal(messages(subject.tree).messages.length, 1);
    await subject.settle();
    assert.deepEqual(messages(subject.tree).messages.map(message => message.text), ["older", "latest"]);
    assert.equal(root.scrollTop, 700, "Only the 500px message-list increase belongs in prepend compensation");
  } finally { subject.unmount(); }
});
