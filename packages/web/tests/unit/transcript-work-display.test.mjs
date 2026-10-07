import assert from "node:assert/strict";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createJsxSubjectLoader } from "./jsxSubject.mjs";
import { renderer, nodes } from "./componentHarness.mjs";

const stub = code => `data:text/javascript;base64,${Buffer.from(code).toString("base64")}`;
const loader = createJsxSubjectLoader(new Map([
  ["./useTranscriptCitations", stub('export const useTranscriptCitations=()=>({citations:[],error:false});')],
  ["./useTranscriptTurnMetadata", stub('export const useTranscriptTurnMetadata=()=>new Map();')],
  ["./TranscriptBlockContent", stub('import {createElement} from "' + import.meta.resolve("react") + '"; export const TranscriptBlockRow=({blockId})=>createElement("span",null,blockId); export const TranscriptLiveTail=()=>null; export const TranscriptToolGroupCard=({runEnded})=>createElement("span",{"data-tool-run-ended":String(runEnded)});')],
]));
const { TranscriptBlockList } = await import(await loader(fileURLToPath(new URL("../../src/chat/TranscriptBlockList.tsx", import.meta.url))));

function render(blocks, extra = {}) {
  const list = { blockIds: blocks.map(block => block.blockId), hasOlder: false };
  const store = { subscribeList: () => () => {}, getListSnapshot: () => list,
    subscribeLive: () => () => {}, getLiveSnapshot: () => null,
    getBlockSnapshot: id => blocks.find(block => block.blockId === id) };
  return renderToStaticMarkup(createElement(TranscriptBlockList, { store, sessionId: "session",
    loadingHistory: false, loadingOlderHistory: false, onLoadOlderHistory: async () => {}, ...extra }));
}
const user = { blockId: "user", body: { kind: "userText" }, orderKey: { sourceSequence: "2" } };
const pending = { text: "same input", startedAtMs: 1000, sessionId: "session", agentRunId: "current-run" };
const committedUser = (run, id = "committed") => ({ ...user, blockId: id, presentation: { agentRunId: run } });

test("the work separator is present before and after the first final answer", async () => {
  const { WorkProgress } = await import(await loader(fileURLToPath(new URL("../../src/chat/WorkProgress.tsx", import.meta.url))));
  for (const finalStarted of [false, true]) {
    const markup = renderToStaticMarkup(createElement(WorkProgress, { running: !finalStarted, finalStarted, startedAtMs: 1000 }));
    assert.equal((markup.match(/workProgressHeader/g) || []).length, 1);
  }
});

test("the pending input reserves the metadata row without displaying fake copy controls", () => {
  const markup = render([], { pendingUserMessage: pending });
  assert.match(markup, /class="workspaceUserMessageMeta workspaceUserMessageMetaPlaceholder" aria-hidden="true"/);
});

test("admission preserves the timer reconciliation identity and start time, including a new Session", async context => {
  const harness = new URL("./componentHarness.mjs", import.meta.url).href;
  const lifecycleLoader = createJsxSubjectLoader(new Map([
    ["react", stub(`export * from ${JSON.stringify(import.meta.resolve("react"))}; export {useState,useRef,useMemo,useCallback,useEffect,useLayoutEffect} from ${JSON.stringify(harness)}; export const useSyncExternalStore=(_subscribe,read)=>read();`)],
    ["./useTranscriptCitations", stub('export const useTranscriptCitations=()=>({citations:[],error:false});')],
    ["./useTranscriptTurnMetadata", stub('export const useTranscriptTurnMetadata=()=>new Map();')],
    ["./TranscriptBlockContent", stub('export const TranscriptBlockRow=()=>null; export const TranscriptLiveTail=()=>null; export const TranscriptToolGroupCard=()=>null;')],
  ]));
  const { TranscriptBlockList: List } = await import(await lifecycleLoader(fileURLToPath(new URL("../../src/chat/TranscriptBlockList.tsx", import.meta.url))));
  let blocks = [];
  let snapshot = { blockIds: [], hasOlder: false, viewEpoch: 1 };
  const store = { subscribeList: () => () => {}, getListSnapshot: () => snapshot,
    subscribeLive: () => () => {}, getLiveSnapshot: () => null, subscribeManagedContent: () => () => {},
    getBlockSnapshot: id => blocks.find(block => block.blockId === id) };
  const props = { store, sessionId: null, pendingUserMessage: { ...pending, sessionId: null, agentRunId: null }, running: true, startedAtMs: pending.startedAtMs };
  const oldWindow = globalThis.window;
  globalThis.window = { requestAnimationFrame() { return 1; }, cancelAnimationFrame() {} };
  const subject = renderer(List.type, props, {});
  context.after(() => { subject.unmount(); globalThis.window = oldWindow; });
  const identity = () => {
    const turn = nodes(subject.tree, node => node.props.className === "workspaceTranscriptTurn").at(-1);
    const work = nodes(turn, node => node.type.name === "WorkProgress")[0];
    assert.ok(work, "the same work period remains displayed");
    return [turn.key, work.key, work.type, work.props.startedAtMs];
  };
  await subject.settle(); const before = identity();
  props.sessionId = "session"; props.pendingUserMessage = pending;
  await subject.settle(); assert.deepEqual(identity(), before, "acquiring the durable Session and run IDs must not remount the timer");
  blocks = [committedUser("current-run")]; snapshot = { ...snapshot, blockIds: ["committed"] };
  await subject.settle(); assert.deepEqual(identity(), before);
  props.pendingUserMessage = null;
  await subject.settle(); assert.deepEqual(identity(), before, "clearing the accepted placeholder must not remount the timer");
  const admitted = nodes(subject.tree, node => node.props.className === "workspaceTranscriptTurn")[0];
  props.pendingUserMessage = { ...pending, startedAtMs: 2000, agentRunId: "next-run" };
  await subject.settle();
  assert.equal(nodes(subject.tree, node => node.props.className === "workspaceTranscriptTurn")[0].key, admitted.key,
    "the next send must not remount the preceding work period");
});

test("accepted input stays visible until its own committed user message arrives", () => {
  for (const blocks of [[], [committedUser("previous-run")], [user]]) {
    const markup = render(blocks, { pendingUserMessage: pending });
    assert.match(markup, /data-block-id="pending:user"/);
    assert.equal((markup.match(/workProgress.working/g) || []).length, 1);
  }
});

test("a shorter authoritative tail replaces the pending bubble and timer by run identity", () => {
  // The former page held many user messages; a tail reload need not increase that count.
  const markup = render([committedUser("current-run")], {
    pendingUserMessage: { ...pending, baselineUserBlocks: 10 }, running: true, startedAtMs: 1000,
  });
  assert.doesNotMatch(markup, /data-block-id="pending:user"/);
  assert.equal((markup.match(/workProgress.working/g) || []).length, 1);
  assert.doesNotMatch(markup, /workProgress.worked/);
});

test("identical consecutive input text does not acknowledge a different run", () => {
  const old = { ...committedUser("previous-run"), body: { kind: "userText", content: { inlineContent: pending.text, sourceRef: null } } };
  assert.match(render([old], { pendingUserMessage: pending }), /data-block-id="pending:user"/);
  assert.doesNotMatch(render([old, committedUser("current-run", "new")], { pendingUserMessage: pending }), /data-block-id="pending:user"/);
});

test("an acceptance receipt alone and another Session cannot replace or leak an input", () => {
  assert.match(render([user], { pendingUserMessage: { ...pending, agentRunId: null } }), /data-block-id="pending:user"/);
  assert.doesNotMatch(render([], { pendingUserMessage: { ...pending, sessionId: "other-session" } }), /data-block-id="pending:user"/);
});

test("an idle request without work evidence has no completed work label", () => {
  const markup = render([user]);
  assert.ok(markup.includes("user"));
  assert.ok(!markup.includes("workProgress.worked"));
});
test("one real failure keeps its visible content and one work period", () => {
  const boundary = { blockId: "empty", body: { kind: "notice", noticeType: "run_boundary", content: { inlineContent: "", sourceRef: null } } };
  const failure = { blockId: "failure", body: { kind: "notice", noticeType: "run_boundary", content: { inlineContent: "error", sourceRef: null } }, orderKey: { sourceSequence: "3" } };
  const markup = render([boundary, user, failure], { startedAtMs: 1000, completedAtMs: 10000 });
  assert.equal((markup.match(/workProgress.worked/g) || []).length, 1);
  assert.ok(markup.includes("failure"));
});
test("an active or pending request has one working indicator even without process content", () => {
  assert.equal((render([user], { running: true }).match(/workProgress.working/g) || []).length, 1);
  assert.equal((render([], { pendingUserMessage: { ...pending, agentRunId: null } }).match(/workProgress.working/g) || []).length, 1);
});

test("confirmed terminal time is passed to unresolved tool groups without inventing a receipt", () => {
  const tool = { blockId: "tool", body: { kind: "tool", status: "running" }, orderKey: { sourceSequence: "3" } };
  assert.match(render([user, tool], { completedAtMs: 10000 }), /data-tool-run-ended="true"/);
  assert.match(render([user, tool], { running: true }), /data-tool-run-ended="false"/);
});
