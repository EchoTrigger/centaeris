import assert from "node:assert/strict";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { button, deferred, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";

const harness = new URL("./componentHarness.mjs", import.meta.url).href;
const stub = code => `data:text/javascript;base64,${Buffer.from(code).toString("base64")}`;
const loader = subjectLoader({ extraOverrides: [
  ["react-router", stub(`export {matchPath} from ${JSON.stringify(import.meta.resolve("react-router"))}; import {context} from ${JSON.stringify(harness)}; export const Link=()=>null; export const useRouteLoaderData=id=>id==='authenticated'?{user:context.user}:{workspace:context.workspace,agents:context.agents}; export const useNavigate=()=>context.navigate;`)],
  ["../i18n", stub("export const t=key=>key; export const i18n={language:'en'}; export const useTranslation=()=>({t});")],
  ["react", stub(`export * from ${JSON.stringify(import.meta.resolve("react"))}; export {useState,useRef,useMemo,useCallback,useEffect} from ${JSON.stringify(harness)}; import {useEffect} from ${JSON.stringify(harness)}; export const useLayoutEffect=useEffect; export const useEffectEvent=callback=>callback; export const useSyncExternalStore=(_subscribe,getSnapshot)=>getSnapshot();`)],
  ["../chat/operationReceipts", stub(`import {context} from ${JSON.stringify(harness)}; export class OperationClient {pending(){return context.pendingOperation ?? null;} submit(...args){return context.submit(...args);} complete(){}} export const acceptedConversationReviewLink=()=>null; export const consumeReviewedOperation=()=>{}; export {loadAcceptedConversation} from ${JSON.stringify(new URL("../../src/chat/operationReceipts.ts", import.meta.url).href)};`)],
  ["../preferences", stub("export const useEnterStartsNewLine=()=>false; export const readModelThinkingMode=()=>''; export const readPreferredModelIdentity=()=>''; export const writeModelThinkingMode=()=>{}; export const writePreferredModelIdentity=()=>{};")],
  ["../chat/transcriptTransport", stub(`import {context} from ${JSON.stringify(harness)}; export const createWorkspaceTranscriptTransport=()=>context.transport;`)],
  ["../chat/workspaceWebTransport", stub(`import {context} from ${JSON.stringify(harness)}; export const streamWorkspaceAgentRun=options=>context.stream(options);`)],
  ...["../components/WorkspaceContextPanel", "../components/DocumentPreview", "../chat/WorkspaceComposer", "../chat/TranscriptBlockList", "../agent-chat/SessionAgentBreadcrumb", "../shell/ShellSidebar"].map(specifier => [specifier, stub(`export const ${specifier.split("/").at(-1)}=()=>null;`)]),
  ["../shell/HomePlane", stub("export const HomePlane=()=>null; export const HomeQuickActions=()=>null;")],
] });
const { AppPageContent } = await import(await loader(fileURLToPath(new URL("../../src/routes/AppRoute.jsx", import.meta.url))));
const component = (tree, name) => nodes(tree, node => node.type.name === name)[0];
const page = text => ({
  schema: "transcript.page.v1", sessionId: "session", projectionVersion: "transcript.projection.v1",
  projectionGeneration: "generation", sourceHighWater: "99", olderCursor: null, hasOlder: false,
  resumeCursors: [{ streamId: "workspace-transcript.v1", cursor: "99" }], blocks: [{ blockId: "answer", blockRevision: "1", orderKey: { sourceSequence: "99", ordinal: 0 },
    body: { kind: "assistantText", content: { inlineContent: text, sourceRef: null }, status: "completed" } }],
});

async function setup(context, tailPage = page("Retained authoritative answer"), idle = false) {
  const oldWindow = globalThis.window;
  globalThis.window = { addEventListener() {}, removeEventListener() {}, requestAnimationFrame() {}, location: { pathname: "/w/workspace/agents/agent" } };
  context.after(() => { globalThis.window = oldWindow; });
  const failures = [];
  context.mock.method(console, "error", (...values) => failures.push(values));
  const attempt = deferred();
  const session = { id: "session", workspaceId: "workspace", agentId: "agent", projectId: null, title: "Synthetic Session", origin: "user", status: "active", deletedAt: null, isPinned: false, isUnread: false, hasActiveAgentRun: true, updatedAt: "2026-10-05T00:00:00Z", initialInputOrigin: null };
  const env = environment({ location: { search: "?sessionId=session", state: null },
    request: async path => ({ json: async () => path === "/api/models" ? { models: idle ? [{ id: "model", providerId: "provider", modelName: "synthetic", displayName: "Synthetic", thinkingModes: [] }] : [] }
      : path.includes("session-projects") ? { projects: [] }
      : path.endsWith("/assets") ? { assets: [] }
      : path === "/api/sessions/session" ? { session } : { sessions: [session] } }),
    transport: { loadTail: async () => tailPage, loadPatches: async () => "99",
      loadActiveAgentRun: async () => ({ agentRun: idle ? null : { agentRunId: "run", streamCursor: "99" } }) },
    stream: options => { env.streamOptions = options; return attempt.promise; },
  });
  const subject = renderer(AppPageContent, { agentId: "agent", workspaceDraft: false, location: env.location, modelsVersion: 0 }, env);
  await subject.settle();
  if (!idle) assert.ok(env.streamOptions, `the selected Session started its stream; ${JSON.stringify(nodes(subject.tree, node => node.props.className === 'errorBanner').map(node => node.props.children))}`);
  const list = () => component(subject.tree, "TranscriptBlockList");
  const composer = () => component(subject.tree, "WorkspaceComposer");
  const store = list().props.store;
  composer().props.onDraftChange("Unsent draft"); await subject.settle();
  const preserved = () => {
    assert.equal(store.getBlockSnapshot("answer").body.content.inlineContent, "Retained authoritative answer");
    assert.equal(composer().props.draft, "Unsent draft");
  };
  context.after(() => subject.unmount());
  return { subject, env, attempt, failures, preserved, store, list, composer };
}

test("Session send binds its pending input to the accepted run before replacing a shorter tail", async context => {
  const previous = [1, 2, 3].map(index => ({ blockId: `old-${index}:user`, blockRevision: "1",
    orderKey: { sourceSequence: String(index), ordinal: 0 }, body: { kind: "userText", content: { inlineContent: "same input", sourceRef: null } } }));
  const { subject, env, list, composer, store } = await setup(context, historyPage(previous, null), true);
  const acceptance = deferred(); const tail = deferred();
  env.submit = () => acceptance.promise;
  composer().props.onDraftChange("same input"); await subject.settle();
  const send = composer().props.onSubmit({ preventDefault() {} }); await subject.settle();
  const startedAtMs = list().props.pendingUserMessage.startedAtMs;
  assert.equal(list().props.pendingUserMessage.agentRunId, null);
  env.transport.loadTail = () => tail.promise;
  env.transport.loadActiveAgentRun = async () => ({ agentRun: { agentRunId: "accepted-run", streamCursor: "100" } });
  acceptance.resolve({ sessionId: "session", agentRunId: "accepted-run", operationId: "operation" });
  await subject.settle();
  assert.deepEqual(list().props.pendingUserMessage, { text: "same input", startedAtMs, sessionId: "session", agentRunId: "accepted-run" });
  assert.equal(store.getListSnapshot().blockIds.length, 3, "acceptance alone preserves the displayed history and pending input");
  tail.resolve(historyPage([{ ...previous[0], blockId: "new:user", orderKey: { sourceSequence: "100", ordinal: 0 },
    presentation: { agentRunId: "accepted-run", sourceType: "user_message", observedAtMs: 1000, displayTarget: null, durationMs: null, operation: null } }], null, "100"));
  await send; await subject.settle();
  assert.equal(list().props.pendingUserMessage, null);
  assert.deepEqual(store.getListSnapshot().blockIds, ["new:user"]);
  assert.equal(list().props.startedAtMs, startedAtMs, "the running timer keeps its original start through admission");
});

test("normal submission receipt states never add a status banner or recovery buttons", async context => {
  const { subject, env } = await setup(context);
  for (const receipt of [null, { sessionId: "session", operationId: "accepted", agentRunId: "run" }]) {
    env.pendingOperation = { operationId: "accepted", receipt, inputChanged: false };
    await subject.settle();
    assert.equal(nodes(subject.tree, node => node.props.className === "transcriptStreamStatus").length, 0);
    assert.equal(button(subject.tree, "operations.check"), undefined);
    assert.equal(button(subject.tree, "operations.open"), undefined);
  }
});

test("transient Session reconnects preserve content and draft without a visible status banner", async context => {
  const { subject, env, preserved } = await setup(context);
  for (const connection of ["reconnecting", "running", "reconnecting"]) {
    env.streamOptions.onConnection(connection); await subject.settle();
    preserved();
    assert.equal(nodes(subject.tree, node => node.props.className === "transcriptStreamStatus").length, 0);
    assert.equal(button(subject.tree, "appRoute.retryStream"), undefined);
  }
});

function historyBlock(id, sequence, text, revision = "1") {
  return { blockId: id, blockRevision: revision, orderKey: { sourceSequence: String(sequence), ordinal: 0 },
    body: { kind: "assistantText", content: { inlineContent: text, sourceRef: null }, status: "completed" } };
}
function historyPage(blocks, cursor, highWater = "99", generation = "generation") {
  return { ...page("unused"), blocks, sourceHighWater: highWater, projectionGeneration: generation,
    olderCursor: cursor, hasOlder: cursor !== null,
    resumeCursors: [{ streamId: "workspace-transcript.v1", cursor: highWater }] };
}
function historyTool(revision, summary) {
  return { blockId: "middle", blockRevision: revision, orderKey: { sourceSequence: "70", ordinal: 0 },
    body: { kind: "tool", callId: "call", toolName: "read_file", status: revision === "1" ? "running" : "completed",
      summary, summaryRef: null, outputRef: revision === "1" ? null : { refId: "tool-output:call", revision: "2", byteLength: "100000" } } };
}
async function pagedHistory(context) {
  const fixture = await setup(context, historyPage([historyBlock("answer", 99, "Retained authoritative answer")], "old-1"));
  fixture.env.transport.loadOlder = async (_identity, cursor) => cursor === "old-1"
    ? historyPage([historyTool("1", "Old middle text")], "old-2")
    : historyPage([historyBlock("reading", 50, "Older message being read"), historyBlock("deleted", 60, "Removed by new projection")], "old-3");
  await fixture.list().props.onLoadOlderHistory(); await fixture.subject.settle();
  await fixture.list().props.onLoadOlderHistory(); await fixture.subject.settle();
  assert.deepEqual(fixture.store.getListSnapshot().blockIds, ["reading", "deleted", "middle", "answer"]);
  const reads = [];
  const nextPage = deferred();
  fixture.env.transport.loadTail = async () => historyPage([historyBlock("answer", 99, "Fresh authoritative answer", "2"), historyBlock("latest", 150, "New answer")], "fresh-1", "155", "generation-new");
  fixture.env.transport.loadOlder = async (identity, cursor, signal) => {
    reads.push({ identity, cursor, signal });
    if (cursor === "fresh-1") return historyPage([historyTool("2", "Fresh middle text")], "fresh-2", "155", "generation-new");
    if (cursor === "fresh-2") return nextPage.promise;
    throw new Error("must not load beyond the previously loaded range");
  };
  fixture.env.transport.loadActiveAgentRun = async () => ({ agentRun: null });
  return { ...fixture, reads, nextPage };
}

test("successful Session resynchronization atomically reloads the loaded older range from the new frozen projection", async context => {
  const { subject, env, store, reads, nextPage, composer } = await pagedHistory(context);
  const before = store.getListSnapshot();
  const published = [];
  store.subscribeList(() => published.push(store.getListSnapshot().blockIds));
  const recovery = env.streamOptions.recover(new Error("transcript_view_invalidated"));
  await subject.settle();
  assert.deepEqual(reads.map(read => read.cursor), ["fresh-1", "fresh-2"], "reload stops at the loaded oldest order rather than downloading all history");
  assert.equal(store.getListSnapshot(), before, "no partially read replacement is visible");
  assert.equal(composer().props.draft, "Unsent draft");
  nextPage.resolve(historyPage([historyBlock("reading", 50, "Fresh older message", "2")], "fresh-3", "155", "generation-new"));
  assert.equal(await recovery, null);
  await subject.settle();
  assert.deepEqual(store.getListSnapshot().blockIds, ["reading", "middle", "answer", "latest"]);
  assert.equal(store.getBlockSnapshot("reading").body.content.inlineContent, "Fresh older message");
  assert.equal(store.getBlockSnapshot("middle").blockRevision, "2", "old-waterline revisions are never merged into the new view");
  assert.equal(store.getBlockSnapshot("middle").body.outputRef.revision, "2", "tool content is bound to the newly read authority");
  assert.equal(store.getBlockSnapshot("deleted"), null, "removed source blocks do not survive generation changes");
  assert.equal(store.getListSnapshot().olderCursor, "fresh-3", "future pagination continues from the restored range");
  assert.equal(composer().props.draft, "Unsent draft");
  assert.deepEqual(published, [["reading", "middle", "answer", "latest"]], "listeners see exactly one complete replacement");
  assert.ok(reads.every(read => read.identity.sourceHighWater === "155" && read.identity.projectionGeneration === "generation-new"));
});

test("failed older-page resynchronization preserves every loaded message and draft", async context => {
  const { subject, env, store, reads, nextPage, composer } = await pagedHistory(context);
  const before = store.getListSnapshot();
  const recovery = env.streamOptions.recover(new Error("transcript_view_invalidated"));
  await subject.settle();
  assert.deepEqual(reads.map(read => read.cursor), ["fresh-1", "fresh-2"]);
  const unavailable = new Error("transcript_page_unavailable");
  nextPage.reject(unavailable);
  await assert.rejects(recovery, error => error === unavailable);
  await subject.settle();
  assert.equal(store.getListSnapshot(), before);
  assert.equal(store.getBlockSnapshot("middle").body.summary, "Old middle text");
  assert.equal(store.getBlockSnapshot("reading").body.content.inlineContent, "Older message being read");
  assert.equal(composer().props.draft, "Unsent draft");
});

test("successful resynchronization cancels an older request and ignores its late old-waterline page", async context => {
  const { subject, env, store, nextPage, list } = await pagedHistory(context);
  const oldRequest = deferred();
  const freshRead = env.transport.loadOlder;
  let oldSignal;
  env.transport.loadOlder = (identity, cursor, signal) => {
    if (cursor !== "old-3") return freshRead(identity, cursor, signal);
    oldSignal = signal;
    return oldRequest.promise;
  };
  const oldLoad = list().props.onLoadOlderHistory();
  await subject.settle();
  const recovery = env.streamOptions.recover(new Error("transcript_view_invalidated"));
  await subject.settle();
  nextPage.resolve(historyPage([historyBlock("reading", 50, "Fresh older message", "2")], "fresh-3", "155", "generation-new"));
  await recovery; await subject.settle();
  assert.equal(oldSignal.aborted, true);
  oldRequest.resolve(historyPage([historyBlock("late-old", 40, "Late obsolete page")], null));
  await oldLoad; await subject.settle();
  assert.equal(store.getBlockSnapshot("late-old"), null);
  assert.equal(store.getListSnapshot().olderCursor, "fresh-3");
});

for (const unavailable of ["tail", "active run"]) test(`failed ${unavailable} recovery keeps the snapshot and offers a header retry`, async context => {
  const { subject, env, attempt, failures, preserved, store } = await setup(context);
  const stopped = new Error("transcript_page_unavailable");
  attempt.reject(stopped); await subject.settle();
  preserved();
  const retry = button(subject.tree, "appRoute.retryStream");
  const header = nodes(subject.tree, node => node.type === "header")[0];
  assert.ok(retry, "exhausted recovery remains actionable");
  assert.ok(nodes(header, node => node === retry).length, "retry belongs in the existing header");
  assert.equal(nodes(subject.tree, node => node.props.className === "transcriptStreamStatus").length, 0);
  assert.equal(failures[0][0], "Transcript stream stopped");
  assert.equal(failures[0][1].error, stopped);
  const before = store.getListSnapshot().viewEpoch;
  if (unavailable === "tail") env.transport.loadTail = async () => { throw stopped; };
  else {
    env.transport.loadTail = async () => page("Uncommitted replacement answer");
    env.transport.loadActiveAgentRun = async () => { throw stopped; };
  }
  await retry.props.onClick(); await subject.settle();
  preserved();
  assert.equal(store.getListSnapshot().viewEpoch, before, "failed replacement reads do not publish a new view");
  assert.ok(button(subject.tree, "appRoute.retryStream"));
});
