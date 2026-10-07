import assert from "node:assert/strict";
import { test } from "node:test";
import { agentHistoryPath, parseAgentHistoryPage, mergeAgentHistory, prependAgentHistory, projectAgentHistory } from "../../src/agent-chat/agentHistory.ts";

// Synthetic authoritative-contract rows. Cursors are opaque to the frontend.
const input = (overrides = {}) => ({ cursor: "opaque-input", kind: "input", input: { inputId: "same-id", sequence: 99, createdAtMs: 2, body: "  Exact input  ", attachments: [], read: null, ...overrides } });
const message = (overrides = {}) => ({ cursor: "opaque-message", kind: "message", message: { id: "same-id", agentRunId: "run", turnId: "turn", toolCallId: "call", sourceSequence: 1, createdAtMs: 1, body: "Whole reply", sessionRefs: ["work"], fileRefs: ["opaque-file"], ...overrides } });
const page = (overrides = {}) => ({ schema: "agent.history.v1", agentId: "agent", sessionId: "coord", items: [input(), message()], nextCursor: "ignored-last-source", newestCursor: "opaque-message", hasMore: false, ...overrides });

test("history renders server order without sorting timestamps or independent input/output sequences", () => {
  const parsed = parseAgentHistoryPage(page(), "agent", null);
  assert.deepEqual(parsed.items.map(row => row.kind), ["input", "message"]);
  assert.equal(parsed.nextCursor, "ignored-last-source");
  const views = projectAgentHistory(parsed.items, "You", "Agent", new Map(), true);
  assert.deepEqual(views.map(view => view.role), ["user", "agent"]);
  assert.deepEqual(views.map(view => view.createdAt), ["1970-01-01T00:00:00.002Z", "1970-01-01T00:00:00.001Z"]);
  assert.equal(views[0].text, "  Exact input  ");
});

test("opaque cursors survive complete query encoding and remain available when hasMore is false", () => {
  const cursor = "all-components/+_=opaque:cursor";
  const url = new URL(agentHistoryPath("agent/one", cursor), "https://example.invalid");
  assert.equal(url.pathname, "/api/agents/agent%2Fone/history");
  assert.equal(url.searchParams.get("beforeCursor"), cursor);
  assert.equal(new URL(agentHistoryPath("agent", cursor, 50, "newer"), url).searchParams.get("afterCursor"), cursor);
  assert.equal(url.searchParams.get("limit"), "50");
  assert.equal(new URL(agentHistoryPath("agent", null), url).searchParams.has("afterCursor"), false);
  assert.equal(parseAgentHistoryPage(page({ items: [], nextCursor: cursor }), "agent", cursor).nextCursor, cursor);
  assert.equal(parseAgentHistoryPage(page({ items: [], nextCursor: "advanced-ignored", hasMore: true }), "agent", cursor).items.length, 0);
});

test("older pages prepend chronological rows independently of the newest polling cursor", () => {
  const latest = parseAgentHistoryPage(page({ nextCursor: "opaque-input" }), "agent", null);
  const earlier = { ...input({ inputId: "earlier", sequence: 1 }), cursor: "earlier" };
  const older = parseAgentHistoryPage(page({ items: [earlier], nextCursor: "earlier", newestCursor: "earlier" }), "agent", latest.nextCursor);
  assert.deepEqual(prependAgentHistory(latest.items, older.items).map(row => row.cursor), ["earlier", "opaque-input", "opaque-message"]);
  assert.equal(latest.newestCursor, "opaque-message");
  assert.throws(() => prependAgentHistory(latest.items, latest.items));
});

test("history binds Agent and coordination identity, exact fields and bounded progress without decoding cursors", () => {
  for (const value of [page({ agentId: "other" }), page({ sessionId: "other" }), page({ schema: "unknown" }), page({ nextAfterSequence: 1 }), page({ hasMore: "yes" }), page({ items: [], nextCursor: null, hasMore: true }), page({ nextCursor: null })]) {
    assert.throws(() => parseAgentHistoryPage(value, "agent", null, "coord"));
  }
  assert.throws(() => parseAgentHistoryPage(page({ items: [], nextCursor: "same", hasMore: true }), "agent", "same"));
  assert.throws(() => parseAgentHistoryPage(page(), "agent", null, "coord", 1));
  assert.throws(() => parseAgentHistoryPage(page({ items: [input(), { ...message(), cursor: "opaque-input" }] }), "agent", null));
  assert.throws(() => parseAgentHistoryPage(page({ items: [{ ...input(), acceptedSourceSequence: 1 }] }), "agent", null));
});

test("history row identity distinguishes an input from an output with the same opaque ID", () => {
  const rows = parseAgentHistoryPage(page(), "agent", null).items;
  assert.equal(mergeAgentHistory([], rows).length, 2);
  assert.equal(mergeAgentHistory(rows, rows).length, 2);
});

test("refresh updates loaded rows in place and rejects changed immutable identity, cursor or canonical body", () => {
  const rows = parseAgentHistoryPage(page(), "agent", null).items;
  const refreshed = parseAgentHistoryPage(page(), "agent", null).items;
  assert.deepEqual(mergeAgentHistory(rows, refreshed).map(row => row.cursor), ["opaque-input", "opaque-message"]);
  assert.throws(() => mergeAgentHistory(rows, parseAgentHistoryPage(page({ items: [input({ body: "altered" })] }), "agent", null).items));
  assert.throws(() => mergeAgentHistory(rows, parseAgentHistoryPage(page({ items: [{ ...input(), cursor: "moved" }] }), "agent", null).items));
  assert.throws(() => mergeAgentHistory(rows, [...rows].reverse()));
});

test("pagination appends supplied order and preserves old rows; cursor reuse for another identity fails", () => {
  const rows = parseAgentHistoryPage(page(), "agent", null).items;
  const next = parseAgentHistoryPage(page({ items: [{ ...input({ inputId: "later", sequence: 100, createdAtMs: 0 }), cursor: "new-input" }], nextCursor: "new-input" }), "agent", "ignored-last-source").items;
  assert.deepEqual(mergeAgentHistory(rows, next).map(row => row.cursor), ["opaque-input", "opaque-message", "new-input"]);
  assert.throws(() => mergeAgentHistory(rows, [{ ...next[0], cursor: "opaque-message" }]));
});

test("null Read never creates an uptake fact and legacy admission shapes loud-fail", () => {
  assert.equal("loopInputFact" in projectAgentHistory(parseAgentHistoryPage(page(), "agent", null).items, "You", "Agent", new Map(), true)[0], false);
  for (const read of [{ agentRunId: "run", factRef: "fact", atMs: 1 }, { kind: "admission" }, true]) assert.throws(() => parseAgentHistoryPage(page({ items: [input({ read })] }), "agent", null));
});

test("canonical Read updates one immutable input in place, cannot regress or change, and only the latest user displays it", () => {
  const read = { agentRunId: "run", eventId: "event", requestId: "request", createdAtMs: 4 };
  const rows = parseAgentHistoryPage(page(), "agent", null).items;
  const refreshed = parseAgentHistoryPage(page({ items: [input({ read })] }), "agent", null).items;
  const merged = mergeAgentHistory(rows, refreshed);
  assert.deepEqual(merged.map(row => row.cursor), rows.map(row => row.cursor));
  assert.deepEqual(projectAgentHistory(merged, "You", "Agent", new Map(), true)[0].loopInputFact, { kind: "loopInput", messageId: "same-id", receivedAt: "1970-01-01T00:00:00.004Z" });
  assert.throws(() => mergeAgentHistory(merged, [rows[0]]));
  assert.throws(() => mergeAgentHistory(merged, parseAgentHistoryPage(page({ items: [input({ read: { ...read, eventId: "other" } })] }), "agent", null).items));
  const later = { ...input({ inputId: "later", sequence: 100, read }), cursor: "later" };
  const views = projectAgentHistory(mergeAgentHistory(merged, [later]), "You", "Agent", new Map(), true);
  assert.deepEqual(views.map(view => view.isLatest), [false, true, true]);
});

test("latest-by-role is selected only at a complete loaded tail and references retain authorized host identity", () => {
  const parsed = parseAgentHistoryPage(page({ items: [input(), message(), { ...input({ inputId: "last", sequence: 100 }), cursor: "last" }], nextCursor: "opaque-input", newestCursor: "last" }), "agent", null);
  const sessions = new Map([["work", { sessionId: "work", title: "Actual title", runState: "unknown" as const }]]);
  assert.deepEqual(projectAgentHistory(parsed.items, "You", "Agent", sessions, false).map(view => view.isLatest), [false, false, false]);
  const views = projectAgentHistory(parsed.items, "You", "Agent", sessions, true);
  assert.deepEqual(views.map(view => view.isLatest), [false, true, true]);
  assert.equal(views[1].role === "agent" && views[1].sessions?.[0].title, "Actual title");
  assert.equal("libraryObjectId" in views[1], false);
});
