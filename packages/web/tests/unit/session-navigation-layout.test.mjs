import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { button, deferred, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";

// Public callbacks and rendered navigation, using synthetic facts only. This
// harness characterizes behavior; it does not measure browser layout or focus.
const url = path => fileURLToPath(new URL(path, import.meta.url));
const stub = code => `data:text/javascript;base64,${Buffer.from(code).toString("base64")}`;
const { ShellPage } = await import(await subjectLoader({ extraOverrides: [["./ShellSidebar", stub("export const ShellSidebar=()=>null;")]] })(url("../../src/shell/ShellPage.jsx")));
const harness = new URL("./componentHarness.mjs", import.meta.url).href;
const appLoader = subjectLoader({ extraOverrides: [
  ["react-router", stub(`export {matchPath} from ${JSON.stringify(import.meta.resolve("react-router"))}; import {context} from ${JSON.stringify(harness)}; export const Link=()=>null; export const useRouteLoaderData=id=>id==='authenticated'?{user:context.user}:{workspace:context.workspace}; export const useNavigate=()=>context.navigate;`)],
  ["../i18n", stub("export const t=key=>key; export const i18n={language:'en'}; export const useTranslation=()=>({t});")],
  ["react", stub(`export * from ${JSON.stringify(import.meta.resolve("react"))}; export {useState,useRef,useMemo,useCallback,useEffect} from ${JSON.stringify(harness)}; import {useEffect} from ${JSON.stringify(harness)}; export const useLayoutEffect=useEffect; export const useEffectEvent=callback=>callback; export const useSyncExternalStore=(_subscribe,getSnapshot)=>getSnapshot();`)],
  ["../chat/operationReceipts", stub("export class OperationClient {pending(){return null;}} export const acceptedConversationReviewLink=()=>null; export const consumeReviewedOperation=()=>{}; export const loadAcceptedConversation=()=>{};")],
  ["../preferences", stub("export const useEnterStartsNewLine=()=>false; export const readModelThinkingMode=()=>''; export const readPreferredModelIdentity=()=>''; export const writeModelThinkingMode=()=>{}; export const writePreferredModelIdentity=()=>{};")],
  ["../chat/transcriptTransport", stub("export const createWorkspaceTranscriptTransport=()=>({});")],
  ["../chat/workspacePresentationTransport", stub(`import {context} from ${JSON.stringify(harness)}; export const streamWorkspacePresentation=options=>context.presentation?.(options)??new Promise(resolve=>options.signal.addEventListener("abort",resolve,{once:true}));`)],
  ...["../components/WorkspaceContextPanel", "../components/DocumentPreview", "../chat/WorkspaceComposer", "../chat/TranscriptBlockList", "../shell/ShellSidebar"].map(specifier => [specifier, stub(`export const ${specifier.split("/").at(-1)}=()=>null;`)]),
  ["../shell/HomePlane", stub("export const HomePlane=()=>null; export const HomeQuickActions=()=>null;")],
] });
const { AppPageContent } = await import(await appLoader(url("../../src/routes/AppRoute.jsx")));
const { useSessionNavigationActions } = await import(await subjectLoader()(url("../../src/shell/useSessionNavigationActions.js")));
const session = { currentEnvironmentId:null, executionMode:null, deletedAt:null,  isUnread:false, isManuallyUnread:false, latestAttentionSequence:"0", readThroughSequence:"0", attentionRevision:"0", id: "work", workspaceId: "workspace", title: "Synthetic work", status: "active", origin: "user", isPinned: false, projectId: null, hasActiveAgentRun: false, updatedAt: "2026-10-01T00:00:00Z" };
const component = (tree, name) => nodes(tree, node => node.type.name === name)[0];

test("ordinary Session rows retain running, unread and pin indicators and all metadata actions", async context => {
  const previousWindow = globalThis.window; const previousStorage = globalThis.sessionStorage;
  globalThis.window = { addEventListener() {}, removeEventListener() {}, requestAnimationFrame() {} };
  globalThis.sessionStorage = { getItem: () => null };
  context.after(() => { globalThis.window = previousWindow; globalThis.sessionStorage = previousStorage; });
  const item = { ...session, isPinned: true, isUnread: true, isManuallyUnread: true, origin: "automation", hasActiveAgentRun: true };
  const calls = [];
  const env = environment({ location: { search: "?new=1", state: { sidebarTab: "chat" } }, request: async (path, options = {}) => {
    calls.push([path, options]);
    const data = path === "/api/models?workspaceId=workspace" ? { models: [] } : path.includes("session-projects") ? { projects: [] }
      : options.method === "PATCH" ? { session: { ...item, ...JSON.parse(options.body), isManuallyUnread:JSON.parse(options.body).isUnread??item.isManuallyUnread, attentionRevision:"1" } } : { sessions: [item] };
    return { ...data, json: async () => data };
  } });
  const subject = renderer(AppPageContent, { workspaceDraft: false, location: env.location, modelsVersion: 0 }, env);
  const row = () => {
    const value = component(subject.tree, "ShellSidebar").props.sessionProps.renderSessionRow(component(subject.tree, "ShellSidebar").props.sessionProps.sessions[0], { icon: true });
    return typeof value.type === "function" ? renderer(value.type, value.props, env).render() : value;
  };
  const openMenu = async () => { button(row(), "appRoute.conversationActionsForValue").props.onClick(); await subject.settle(); };
  try {
    await subject.settle();
    for (const label of ["appRoute.running", "appRoute.pinned", "appRoute.unread"]) assert.ok(nodes(row(), node => node.props["aria-label"] === label).length);
    assert.equal(nodes(row(), node => node.props.children === "appRoute.auto").length, 0, "Agent work remains an ordinary Session without an automatic badge");
    await openMenu();
    for (const label of ["appRoute.rename", "appRoute.unpin", "appRoute.markAsRead", "appRoute.delete"]) assert.ok(button(row(), label));
    button(row(), "appRoute.unpin").props.onClick(); await subject.settle();
    assert.deepEqual(JSON.parse(calls.find(([, options]) => options.method === "PATCH")[1].body), { isPinned: false });
    await openMenu(); button(row(), "appRoute.rename").props.onClick(); await subject.settle();
    const input = nodes(row(), node => node.type === "input")[0];
    assert.equal(input.props.defaultValue, item.title);
    input.props.onBlur({ currentTarget: { value: " Renamed work " } }); await subject.settle();
    assert.deepEqual(JSON.parse(calls.filter(([, options]) => options.method === "PATCH").at(-1)[1].body), { title: "Renamed work" });
    await openMenu(); button(row(), "appRoute.rename").props.onClick(); await subject.settle();
    const cancelInput = nodes(row(), node => node.type === "input")[0];
    const cancelledTarget = { value: "Cancelled name", blur() { cancelInput.props.onBlur({ currentTarget: this }); } };
    const countBeforeCancel = calls.filter(([, options]) => options.method === "PATCH").length;
    cancelInput.props.onKeyDown({ key: "Escape", preventDefault() {}, currentTarget: cancelledTarget }); await subject.settle();
    assert.equal(calls.filter(([, options]) => options.method === "PATCH").length, countBeforeCancel, "Escape cancels the rename without a metadata command");
    await openMenu(); button(row(), "appRoute.delete").props.onClick(); await subject.settle();
    const dialog = component(subject.tree, "ConfirmDialog");
    assert.equal(dialog.props.open, true);
    assert.equal(calls.some(([, options]) => options.method === "DELETE"), false, "delete remains protected by confirmation");
    await dialog.props.onConfirm(); await subject.settle();
    assert.equal(calls.filter(([, options]) => options.method === "DELETE").length, 1);
  } finally { subject.unmount(); }
});

test("the Session sidebar refreshes background work without changing the open draft or rereading projects", async context => {
  const previousStorage = globalThis.sessionStorage;
  const stored = new Map();
  globalThis.sessionStorage = { getItem:key=>stored.get(key)??null, setItem:(key,value)=>stored.set(key,value), removeItem:key=>stored.delete(key) };
  context.after(() => { globalThis.sessionStorage = previousStorage; });
  const previousWindow = globalThis.window;
  globalThis.window = { addEventListener() {}, removeEventListener() {}, requestAnimationFrame() {} };
  context.after(() => { globalThis.window = previousWindow; });
  const calls = []; let subscription;
  let records = [{ ...session, hasActiveAgentRun: true }];
  const env = environment({ presentation:options=>{subscription=options;return new Promise(resolve=>options.signal.addEventListener("abort",resolve,{once:true}));}, location: { search: "?new=1", state: null }, request: async path => {
    calls.push(path);
    const data = path === "/api/models?workspaceId=workspace" ? { models: [] } : path.includes("session-projects") ? { projects: [] } : { sessions: records };
    return { json: async () => data };
  } });
  const subject = renderer(AppPageContent, { workspaceDraft: false, location: env.location, modelsVersion: 0 }, env);
  try {
    await subject.settle();
    component(subject.tree, "WorkspaceComposer").props.onDraftChange("Keep this unsent draft");
    await subject.settle();
    records = [{ ...session, title: "Completed background work", hasActiveAgentRun: false, isUnread: true, latestAttentionSequence:"1",attentionRevision:"1" }, { ...session, id: "new-work", title: "New background work" }];
    await subscription.controller.acceptWithBackpressure({cursor:null,item:{schema:"workspace.presentation.v1",kind:"navigation",workspaceId:"workspace",sessionId:null,snapshotId:"background",offset:0,hasMore:false,sessions:records}});
    await subject.settle();
    const sidebar = component(subject.tree, "ShellSidebar").props.sessionProps;
    assert.deepEqual(sidebar.sessions, records);
    assert.equal(sidebar.renderSessionRow(records[0]).props.running, false);
    assert.equal(component(subject.tree, "WorkspaceComposer").props.draft, "Keep this unsent draft");
    assert.equal(calls.filter(path => path === "/api/workspaces/workspace/sessions").length, 1, "background changes arrive on the display subscription");
    assert.equal(calls.filter(path => path.includes("session-projects")).length, 1);
    assert.equal(calls.some(path => path.includes("/transcript")), false);
  } finally { subject.unmount(); }
});

test("a selected unread Session is marked read before the new list commits and after an existing mutation", async () => {
  const pending = deferred(); const calls = []; const errors = [];
  const selected = { ...session, id: "selected", isUnread: true, isManuallyUnread: true };
  const env = environment({ request: async (path, options = {}) => {
    const metadata = JSON.parse(options.body); calls.push([path, metadata]);
    if (path.endsWith("/work")) return pending.promise;
    return { session: { ...selected, ...metadata, isManuallyUnread:metadata.isUnread??selected.isManuallyUnread, attentionRevision:"1" } };
  } });
  const props = { scopeKey: "first", sessions: [], onChangeSessions: update => { props.sessions = update(props.sessions); }, onError: error => errors.push(error) };
  const subject = renderer(useSessionNavigationActions, props, env);
  try {
    await subject.settle();
    subject.tree.markSessionRead(selected); await subject.settle();
    assert.deepEqual(calls, [["/api/sessions/selected", { isUnread: false }]], "the observed server Session is valid before setSessions is committed");
    assert.deepEqual(errors, []);
    props.sessions = [session, selected]; await subject.settle();
    const changing = subject.tree.updateSession(session.id, { isPinned: true });
    subject.tree.markSessionRead(selected); await subject.settle();
    assert.equal(calls.length, 2, "wait for the metadata write already in flight");
    pending.resolve({ session: { ...session, isPinned: true } }); await changing; await subject.settle();
    assert.deepEqual(calls.at(-1), ["/api/sessions/selected", { isUnread: false }]);
    assert.equal(calls.length, 3);
  } finally { subject.unmount(); }
});

test("background Session deletion completes against the current conversation selection", async () => {
  const pending = deferred(); const navigation = [];
  const props = { scopeKey: "first", sessions: [session], onChangeSessions() {}, onError() {}, onDeleted: () => navigation.push("old conversation") };
  const subject = renderer(useSessionNavigationActions, props, environment({ request: async () => pending.promise }));
  try {
    await subject.settle(); const deleting = subject.tree.deleteSession(session.id);
    props.onDeleted = () => navigation.push("current conversation"); await subject.settle();
    pending.resolve({}); await deleting; await subject.settle();
    assert.deepEqual(navigation, ["current conversation"]);
  } finally { subject.unmount(); }
});

test("pushed workspace navigation updates sessions without rereading the list or project catalog", async () => {
  const calls = [];
  const env = environment({ request: async path => { calls.push(path); return path.includes("session-projects") ? { projects: [] } : { sessions: [] }; } });
  const item = { id: "work", workspaceId: "workspace", projectId: null, title: "Initial",
    status: "active", updatedAt: "2026-10-05T00:00:00Z", hasActiveAgentRun: false };
  const props = { children: null, showRecentSessions: true, presentationNavigation: null };
  const subject = renderer(ShellPage, props, env);
  try {
    await subject.settle();
    assert.deepEqual(calls, ["/api/workspaces/workspace/session-projects"]);
    props.presentationNavigation = [item]; await subject.settle();
    const sidebar = nodes(subject.tree, node => node.type.name === "ShellSidebar")[0];
    assert.equal(sidebar.props.recentNavigation.sessions[0].title, "Initial");
    props.presentationNavigation = [{ ...item, title: "Renamed by stream" }]; await subject.settle();
    assert.equal(nodes(subject.tree, node => node.type.name === "ShellSidebar")[0].props.recentNavigation.sessions[0].title, "Renamed by stream");
    assert.equal(calls.length, 1);
  } finally { subject.unmount(); }
});
