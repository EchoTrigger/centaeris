import assert from "node:assert/strict";
import { test } from "node:test";
import { agentChatTimeline } from "../../src/agent-chat/agentChatTime.ts";
import type { AgentMessageView } from "../../src/agent-chat/agentChatViewTypes.ts";

// Fixed synthetic canonical instants; browser display settings never change them.
const referenceTime = "2026-09-30T18:00:00Z";
const message = (id: string, createdAt?: string): AgentMessageView => ({
  role: "agent", messageId: id, authorLabel: id, text: id, isLatest: false, createdAt,
});
const separators = (messages: readonly AgentMessageView[], clock = referenceTime, locale = "en-US", timeZone = "UTC") =>
  agentChatTimeline(messages, clock, locale, timeZone).flatMap(row => row.separator ? [row.separator] : []);

test("first message starts a group; adjacent messages below twenty minutes do not repeat its separator", () => {
  const messages = [message("first", "2026-09-30T14:30:00Z"), message("next", "2026-09-30T14:49:59.999Z"), message("last", "2026-09-30T15:09:59.998Z")];
  assert.deepEqual(separators(messages).map(time => time.instant), [messages[0].createdAt]);
});

test("the twenty-minute boundary starts a new group, measured against each adjacent input or output", () => {
  const messages = [message("first", "2026-09-30T14:30:00Z"), { ...message("boundary", "2026-09-30T14:50:00Z"), role: "user" as const }, message("within", "2026-09-30T15:09:59.999Z"), message("next boundary", "2026-09-30T15:29:59.999Z")];
  assert.deepEqual(separators(messages).map(time => time.instant), [messages[0].createdAt, messages[1].createdAt, messages[3].createdAt]);
});

test("crossing local or UTC midnight does not force a separator below twenty minutes", () => {
  const localMidnight = [message("before", "2026-10-01T06:59:30Z"), message("after", "2026-10-01T07:00:30Z")];
  assert.equal(separators(localMidnight, referenceTime, "en-US", "America/Los_Angeles").length, 1);
  assert.equal(separators(localMidnight, referenceTime, "en-US", "UTC").length, 1);
  const utcMidnight = [message("before UTC midnight", "2026-09-30T23:59:30Z"), message("after UTC midnight", "2026-10-01T00:00:30Z")];
  assert.equal(separators(utcMidnight, referenceTime, "en-US", "UTC").length, 1);
  assert.equal(separators(utcMidnight, referenceTime, "en-US", "America/Los_Angeles").length, 1);
});

test("labels use today, yesterday or a local date followed by twelve-hour time and localized day period", () => {
  const messages = [message("older", "2026-09-28T14:29:00Z"), message("yesterday", "2026-09-29T14:29:00Z"), message("today", "2026-09-30T14:29:00Z")];
  assert.deepEqual(separators(messages).map(time => time.label), ["Sep 28, 2026 2:29 PM", "Yesterday 2:29 PM", "Today 2:29 PM"]);
  assert.deepEqual(separators(messages, referenceTime, "zh-CN").map(time => time.label), ["2026年9月28日 2:29 下午", "昨天 2:29 下午", "今天 2:29 下午"]);
  const localDate = separators([message("same instant", "2026-09-30T14:29:00Z")], "2026-10-01T00:30:00Z", "en-US", "America/Los_Angeles")[0];
  assert.equal(localDate.label, "Today 7:29 AM");
  assert.equal(localDate.instant, "2026-09-30T14:29:00Z");
});

test("Chinese and English noon and midnight keep date, twelve-hour clock, day-period order", () => {
  const messages = [message("midnight", "2026-09-30T00:00:00Z"), message("noon", "2026-09-30T12:00:00Z"), message("requested time", "2026-09-30T12:29:00Z")];
  assert.deepEqual(separators(messages).map(time => time.label), ["Today 12:00 AM", "Today 12:00 PM", "Today 12:29 PM"]);
  assert.deepEqual(separators(messages, referenceTime, "zh-CN").map(time => time.label), ["今天 12:00 上午", "今天 12:00 下午", "今天 12:29 下午"]);
  assert.equal(separators([messages[2]], referenceTime, "en-GB")[0].label, "Today 12:29 PM");
  assert.equal(separators([message("local midnight", "2026-09-29T16:00:00Z")], referenceTime, "zh-CN", "Asia/Shanghai")[0].label, "昨天 12:00 上午");
  assert.equal(separators([message("local noon", "2026-09-30T04:00:00Z")], referenceTime, "zh-CN", "Asia/Shanghai")[0].label, "昨天 12:00 下午");
});

test("yesterday follows calendar days across DST rather than elapsed twenty-four-hour periods", () => {
  const spring = separators([message("spring yesterday", "2026-03-08T07:30:00Z")], "2026-03-09T06:30:00Z", "en-US", "America/Los_Angeles");
  const autumn = separators([message("autumn yesterday", "2026-11-01T06:30:00Z")], "2026-11-02T07:30:00Z", "en-US", "America/Los_Angeles");
  assert.equal(spring[0].label, "Yesterday 11:30 PM");
  assert.equal(autumn[0].label, "Yesterday 11:30 PM");
});

test("missing or invalid canonical time creates no invented timestamp; a valid next message starts a group", () => {
  const messages = [message("missing"), message("no timezone", "2026-09-30T14:29:00"), message("invalid date", "2026-02-30T14:29:00Z"), message("valid", "2026-09-30T14:29:00Z"), message("invalid", "not-an-instant"), message("valid after unknown", "2026-09-30T14:30:00Z")];
  assert.deepEqual(separators(messages).map(time => time.instant), [messages[3].createdAt, messages[5].createdAt]);
});

test("replayed history preserves server order, canonical instants and grouping without mutating facts", () => {
  const fact = Object.freeze({ kind: "loopInput" as const, messageId: "user", receivedAt: "2026-09-30T14:32:00Z" });
  const messages = Object.freeze([
    Object.freeze(message("later", "2026-09-30T14:35:00Z")),
    Object.freeze({ role: "user" as const, messageId: "user", authorLabel: "user", text: "user", isLatest: true, createdAt: "2026-09-30T14:30:00Z", loopInputFact: fact }),
    Object.freeze(message("last", "2026-09-30T14:31:00Z")),
  ]);
  const first = agentChatTimeline(messages, referenceTime, "en-US", "UTC");
  assert.deepEqual(first, agentChatTimeline(messages, referenceTime, "en-US", "UTC"));
  assert.deepEqual(first.map(row => row.message.messageId), ["later", "user", "last"]);
  assert.equal(first[1].message, messages[1]);
  assert.equal(messages[1].loopInputFact, fact);
  assert.deepEqual(first.flatMap(row => row.separator ? [row.separator.instant] : []), ["2026-09-30T14:35:00Z"]);
  const nextDay = agentChatTimeline(messages, "2026-10-01T18:00:00Z", "en-US", "UTC");
  assert.deepEqual(nextDay.map(row => row.separator?.instant), first.map(row => row.separator?.instant));
  assert.equal(nextDay[0].separator?.label, "Yesterday 2:35 PM");
});
