import assert from "node:assert/strict";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { ApiError, deferred, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";
const harness = new URL("./componentHarness.mjs", import.meta.url).href;
const stub = code => `data:text/javascript;base64,${Buffer.from(code).toString("base64")}`;
const loader = subjectLoader({ extraOverrides: [
  ["../i18n", stub("export const t=key=>key; export const i18n={language:'en'}; export const useTranslation=()=>({t});")],
  ["react-router", stub(`export {matchPath} from ${JSON.stringify(import.meta.resolve("react-router"))}; import {context} from ${JSON.stringify(harness)}; export const Link=()=>null; export const useRouteLoaderData=id=>id==='authenticated'?{user:context.user}:{workspace:context.workspace,agents:context.agents}; export const useNavigate=()=>context.navigate;`)],
  ["react", stub(`export * from ${JSON.stringify(import.meta.resolve("react"))}; export {useState,useRef,useMemo,useCallback,useEffect,useLayoutEffect} from ${JSON.stringify(harness)}; export const useEffectEvent=callback=>callback; export const useSyncExternalStore=(_subscribe,snapshot)=>snapshot();`)],
  ["../chat/operationReceipts", stub("export class OperationClient {pending(){return null;}} export const acceptedConversationReviewLink=()=>null; export const consumeReviewedOperation=()=>{}; export const loadAcceptedConversation=()=>{};")],
  ["../preferences", stub("export const useEnterStartsNewLine=()=>false; export const readModelThinkingMode=()=>''; export const readPreferredModelIdentity=()=>''; export const writeModelThinkingMode=()=>{}; export const writePreferredModelIdentity=()=>{};")],
  ["../chat/transcriptTransport", stub(`import {context} from ${JSON.stringify(harness)}; export const createWorkspaceTranscriptTransport=()=>({loadTail:async id=>context.tail(id),loadPatches:async()=>{},loadActiveAgentRun:async()=>({agentRun:context.activeRun??null})});`)],
  ["../chat/workspacePresentationTransport", stub(`import {context} from ${JSON.stringify(harness)}; export const streamWorkspacePresentation=options=>context.stream(options);`)],
  ...["../components/WorkspaceContextPanel", "../components/DocumentPreview", "../chat/WorkspaceComposer", "../chat/TranscriptBlockList", "../agent-chat/SessionAgentBreadcrumb", "../shell/ShellSidebar"].map(s => [s, stub(`export const ${s.split("/").at(-1)}=()=>null;`)]),
  ["../shell/HomePlane", stub("export const HomePlane=()=>null; export const HomeQuickActions=()=>null;")],
] });
const { AppPageContent } = await import(await loader(fileURLToPath(new URL("../../src/routes/AppRoute.jsx", import.meta.url))));
const origin = { messageId: "initial-work", agentId: "agent", agentName: "Synthetic Agent" };
const session = (id, initialInputOrigin) => ({ id, workspaceId: "workspace", agentId: "agent", projectId: null, currentEnvironmentId: null, executionMode: "perRun", title: id, status: "active", hasActiveAgentRun: false, origin: "automation", initialInputOrigin, deletedAt: null, isPinned: false, isUnread: false, latestAttentionSequence: "0", readThroughSequence: "0", attentionRevision: "0", isManuallyUnread: false, updatedAt: "2026-10-23T00:00:00Z" });
const tail = id => ({ schema: "transcript.page.v1", sessionId: id, projectionVersion: "transcript.projection.v1", projectionGeneration: "generation", sourceHighWater: "1", blocks: [{ blockId: id === "work" ? origin.messageId : "manual-message", blockRevision: "1", orderKey: { sourceSequence: "1", ordinal: 0 }, body: { kind: "userText", content: { inlineContent: id, sourceRef: null } } }], olderCursor: null, hasOlder: false, resumeCursors: [{ streamId: "workspace-transcript.v1", cursor: "1" }] });
const transcript = subject => nodes(subject.tree, n => n.type.name === "TranscriptBlockList")[0].props;
function setup(context, detail, overrides = {}) {
  const previousWindow = globalThis.window, previousDocument = globalThis.document; const previousStorage = globalThis.sessionStorage;
  globalThis.window = { innerWidth: 1280, addEventListener() {}, removeEventListener() {}, requestAnimationFrame() {} };
  globalThis.sessionStorage = { getItem: () => null };
  globalThis.document = { hidden: false, addEventListener() {}, removeEventListener() {} };
  context.after(() => { globalThis.window = previousWindow; globalThis.document = previousDocument; globalThis.sessionStorage = previousStorage; });
  const requests = [];
  const env = environment({ tail, ...overrides, stream: async options => {
    if (options.sessionId) {
      // The stream has the same scoped authorization as the Session read; a
      // rejected authority cannot keep publishing its cached projection.
      await detail(`/api/sessions/${options.sessionId}`);
      const page = await env.tail(options.sessionId);
      const send = (kind, body, cursor = null) => options.controller.acceptWithBackpressure({ cursor, item: {
        schema: "workspace.presentation.v1", workspaceId: "workspace", agentId: "agent", sessionId: options.sessionId, kind, ...body,
      } });
      await send("snapshot", { page });
      await send("checkpoint", { projectionGeneration: page.projectionGeneration, sourceHighWater: page.sourceHighWater,
        agentRun: overrides.activeRun ? { agentRunId: overrides.activeRun.agentRunId, status: "running", startedAt: null, completedAt: null } : null }, `applied:${page.sourceHighWater}`);
    }
    return overrides.stream ? overrides.stream() : new Promise(resolve => options.signal.addEventListener("abort", resolve, { once: true }));
  }, request: async path => {
    requests.push(path);
    let data;
    if (path === "/api/models?workspaceId=workspace") data = { models: [] };
    else if (path.includes("session-projects")) data = { projects: [] };
    else if (path.includes("/workspaces/")) data = { sessions: [session("work", origin), session("manual", null)] };
    else if (path.endsWith("/assets")) data = { assets: [] };
    else data = await detail(path);
    return { ...data, json: async () => data };
  } });
  const props = { agentId: "agent", workspaceDraft: false, location: { search: "?sessionId=work", state: null }, modelsVersion: 0 };
  return { requests, props, subject: renderer(AppPageContent, props, env) };
}
test("ordinary Session receives its authoritative initial input origin from the scoped Session endpoint", async context => {
  const { subject, requests } = setup(context, async () => ({ session: session("work", origin) }));
  try { await subject.settle(); assert.deepEqual(transcript(subject).initialInputOrigin, origin); assert.ok(requests.includes("/api/sessions/work")); }
  finally { subject.unmount(); }
});
test("a Session switch clears the prior Agent sender before a delayed manual Session response", async context => {
  const pending = deferred();
  const { subject, props } = setup(context, path => path === "/api/sessions/work" ? { session: session("work", origin) } : pending.promise);
  try {
    await subject.settle(); assert.deepEqual(transcript(subject).initialInputOrigin, origin);
    props.location = { search: "?sessionId=manual", state: null }; await subject.settle();
    assert.equal(transcript(subject).initialInputOrigin, null);
    pending.resolve({ session: session("manual", null) }); await subject.settle();
    assert.equal(transcript(subject).initialInputOrigin, null); assert.equal(transcript(subject).sessionId, "manual");
  } finally { subject.unmount(); }
});
test("a rejected Session authority refresh clears the retained origin and its conversation projection", async context => {
  let denied = false;
  const { subject, props } = setup(context, async () => { if (denied) throw new ApiError("session_not_found", 404); return { session: session("work", origin) }; });
  try {
    await subject.settle(); assert.deepEqual(transcript(subject).initialInputOrigin, origin);
    denied = true; props.location = { search: "?sessionId=work&projectId=changed", state: null }; await subject.settle();
    assert.equal(transcript(subject).initialInputOrigin, null); assert.equal(transcript(subject).store.getListSnapshot().blockIds.length, 0);
  } finally { subject.unmount(); }
});

test("a live Session authorization rejection clears its previously accepted Agent sender and transcript", async context => {
  const stream = deferred(); context.mock.method(console, "error", () => {});
  const { subject } = setup(context, async () => ({ session: session("work", origin) }), {
    activeRun: { agentRunId: "run", status: "running", streamCursor: "1" }, stream: () => stream.promise,
  });
  try {
    await subject.settle(); assert.deepEqual(transcript(subject).initialInputOrigin, origin);
    stream.reject(new ApiError("session_not_found", 404)); await subject.settle();
    assert.equal(transcript(subject).initialInputOrigin, null); assert.equal(transcript(subject).store.getListSnapshot().blockIds.length, 0);
  } finally { subject.unmount(); }
});
