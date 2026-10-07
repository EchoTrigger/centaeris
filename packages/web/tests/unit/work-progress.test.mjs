import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { createInstance } from "i18next";
import { formatWorkDuration } from "../../src/chat/workDuration.ts";
import { groupTranscriptTurns, shouldLoadEarlier, shouldReleaseLoadedHistory } from "../../src/chat/transcriptTurns.ts";

test("work duration is localized and never pads or prints zero units", async () => {
  const i18n = createInstance();
  const resources = Object.fromEntries(["en", "zh-CN"].map((language) => [language, {
    translation: JSON.parse(readFileSync(new URL(`../../src/locales/${language}.json`, import.meta.url), "utf8")),
  }]));
  await i18n.init({ resources, lng: "zh-CN", keySeparator: false });
  assert.equal(formatWorkDuration(0, i18n.t.bind(i18n)), "");
  assert.equal(formatWorkDuration(80_000, i18n.t.bind(i18n)), "1分 20秒");
  assert.equal(formatWorkDuration(3_601_000, i18n.t.bind(i18n)), "1时 1秒");
  await i18n.changeLanguage("en");
  assert.equal(formatWorkDuration(80_000, i18n.t.bind(i18n)), "1m 20s");
});

test("only a new user message starts a work period; supplements stay in its process", () => {
  const blocks = [
    ["user-a", "userText"], ["reason", "reasoning"], ["extra", "notice"],
    ["tool", "tool"], ["answer", "assistantText"], ["user-b", "userText"],
  ].map(([blockId, kind]) => ({ blockId, body: { kind } }));
  assert.deepEqual(groupTranscriptTurns(blocks.map(b => b.blockId), id => blocks.find(b => b.blockId === id)), [
    { id: "user-a", userBlockId: "user-a", processIds: ["reason", "extra", "tool"], answerIds: ["answer"] },
    { id: "user-b", userBlockId: "user-b", processIds: [], answerIds: [] },
  ]);
});

test("upward intent loads at the top even when no scroll event can fire", () => {
  assert.equal(shouldLoadEarlier(0, true, false), true);
  assert.equal(shouldLoadEarlier(179, true, false), true);
  assert.equal(shouldLoadEarlier(181, true, false), false);
  assert.equal(shouldLoadEarlier(0, false, false), false);
  assert.equal(shouldLoadEarlier(0, true, true), false);
});

test("background history release never interrupts a reader or an older-page load", () => {
  assert.equal(shouldReleaseLoadedHistory(true, true, false), true);
  assert.equal(shouldReleaseLoadedHistory(false, true, false), false);
  assert.equal(shouldReleaseLoadedHistory(true, false, false), false);
  assert.equal(shouldReleaseLoadedHistory(true, true, true), false);
});

test("an empty run boundary does not create a second work period above the user request", () => {
  const blocks = [
    { blockId: "start", body: { kind: "notice", noticeType: "run_boundary", content: { inlineContent: "", sourceRef: null } } },
    { blockId: "user", body: { kind: "userText" } },
    { blockId: "failure", body: { kind: "notice", noticeType: "run_boundary", content: { inlineContent: "The request failed.", sourceRef: null } } },
  ];
  assert.deepEqual(groupTranscriptTurns(blocks.map(block => block.blockId), id => blocks.find(block => block.blockId === id)), [
    { id: "user", userBlockId: "user", processIds: ["failure"], answerIds: [] },
  ]);
});

test("history pages retain real process and failure content when their user request is on an older page", () => {
  const blocks = [
    { blockId: "start", body: { kind: "notice", noticeType: "run_boundary", content: { inlineContent: "", sourceRef: null } } },
    { blockId: "tool", body: { kind: "tool" } },
    { blockId: "failure", body: { kind: "notice", noticeType: "run_boundary", content: { inlineContent: null, sourceRef: { refId: "failure-content" } } } },
  ];
  const read = id => blocks.find(block => block.blockId === id);
  assert.deepEqual(groupTranscriptTurns(blocks.map(block => block.blockId), read), [
    { id: "continued", userBlockId: null, processIds: ["tool", "failure"], answerIds: [] },
  ]);
  const user = { blockId: "user", body: { kind: "userText" } }; blocks.splice(1, 0, user);
  assert.deepEqual(groupTranscriptTurns(blocks.map(block => block.blockId), read), [
    { id: "user", userBlockId: "user", processIds: ["tool", "failure"], answerIds: [] },
  ]);
});

test("a pending run with only an empty boundary contributes no phantom completed work period", () => {
  const start = { body: { kind: "notice", noticeType: "run_boundary", content: { inlineContent: "", sourceRef: null } } };
  assert.deepEqual(groupTranscriptTurns(["start"], () => start), []);
});
import { initialWorkDisclosure, observeFinalAnswer, toggleWorkDisclosure, preserveWorkDisclosure } from "../../src/chat/workDisclosure.ts";

test("process starts open and folds on first final answer", () => {
  const working = initialWorkDisclosure(false);
  assert.equal(working.expanded, true);
  assert.equal(observeFinalAnswer(working, true).expanded, false);
  assert.equal(initialWorkDisclosure(true).expanded, false);
});

test("manual reopening survives answer updates and projection refresh", () => {
  let state = toggleWorkDisclosure(observeFinalAnswer(initialWorkDisclosure(false), true));
  state = observeFinalAnswer(state, true);
  state = observeFinalAnswer(state, false);
  state = observeFinalAnswer(state, true);
  assert.equal(state.expanded, true);
});

test("manual closure survives refresh; new turns have their own default", () => {
  assert.equal(observeFinalAnswer(initialWorkDisclosure(true), false).expanded, false);
  assert.equal(initialWorkDisclosure(false).expanded, true);
});

test("opening process details before the final answer protects the reader", () => {
  const state = preserveWorkDisclosure(initialWorkDisclosure(false));
  assert.equal(observeFinalAnswer(state, true).expanded, true);
});
