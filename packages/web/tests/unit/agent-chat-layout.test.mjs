import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { ApiError, button, deferred, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";

// Public callbacks and rendered navigation, using synthetic facts only. This
// harness characterizes behavior; it does not measure browser layout or focus.
const url = path => fileURLToPath(new URL(path, import.meta.url));
const { AgentChatPageContent } = await import(await subjectLoader()(url("../../src/routes/AgentChatRoute.tsx")));
const stub = code => `data:text/javascript;base64,${Buffer.from(code).toString("base64")}`;
const { ShellPage } = await import(await subjectLoader({ extraOverrides: [["./ShellSidebar", stub("export const ShellSidebar=()=>null;")]] })(url("../../src/shell/ShellPage.jsx")));
const { ShellSidebar } = await import(await subjectLoader()(url("../../src/shell/ShellSidebar.jsx")));
const harness = new URL("./componentHarness.mjs", import.meta.url).href;
const appLoader = subjectLoader({ extraOverrides: [
  ["react-router", stub(`export {matchPath} from ${JSON.stringify(import.meta.resolve("react-router"))}; import {context} from ${JSON.stringify(harness)}; export const Link=()=>null; export const useRouteLoaderData=id=>id==='authenticated'?{user:context.user}:{workspace:context.workspace,agents:context.agents}; export const useNavigate=()=>context.navigate;`)],
  ["../i18n", stub("export const t=key=>key; export const i18n={language:'en'}; export const useTranslation=()=>({t});")],
  ["react", stub(`export * from ${JSON.stringify(import.meta.resolve("react"))}; export {useState,useRef,useMemo,useCallback,useEffect} from ${JSON.stringify(harness)}; import {useEffect} from ${JSON.stringify(harness)}; export const useLayoutEffect=useEffect; export const useEffectEvent=callback=>callback; export const useSyncExternalStore=(_subscribe,getSnapshot)=>getSnapshot();`)],
  ["../chat/operationReceipts", stub("export class OperationClient {pending(){return null;}} export const acceptedConversationReviewLink=()=>null; export const consumeReviewedOperation=()=>{}; export const loadAcceptedConversation=()=>{};")],
  ["../preferences", stub("export const useEnterStartsNewLine=()=>false; export const readModelThinkingMode=()=>''; export const readPreferredModelIdentity=()=>''; export const writeModelThinkingMode=()=>{}; export const writePreferredModelIdentity=()=>{};")],
  ["../chat/transcriptTransport", stub("export const createWorkspaceTranscriptTransport=()=>({});")],
  ...["../components/WorkspaceContextPanel", "../components/DocumentPreview", "../chat/WorkspaceComposer", "../chat/TranscriptBlockList", "../agent-chat/SessionAgentBreadcrumb", "../shell/ShellSidebar"].map(specifier => [specifier, stub(`export const ${specifier.split("/").at(-1)}=()=>null;`)]),
  ["../shell/HomePlane", stub("export const HomePlane=()=>null; export const HomeQuickActions=()=>null;")],
] });
const { AppPageContent } = await import(await appLoader(url("../../src/routes/AppRoute.jsx")));
const { useSessionNavigationActions } = await import(await subjectLoader()(url("../../src/shell/useSessionNavigationActions.js")));
const row = { kind: "message", cursor: "reply", message: { id: "reply", agentRunId: "run", turnId: "turn", toolCallId: "call", sourceSequence: 1, createdAtMs: 1, body: "Synthetic reply", sessionRefs: ["work"], fileRefs: [] } };
const history = (agentId = "agent", items = [row]) => ({ schema: "agent.history.v1", agentId, sessionId: `coord-${agentId}`, items, nextCursor: "tail", newestCursor: "tail", hasMore: false });
const session = { id: "work", workspaceId: "workspace", agentId: "worker", title: "Synthetic work", status: "active", origin: "user", isPinned: false, projectId: null, hasActiveAgentRun: false, updatedAt: "2026-10-01T00:00:00Z" };
const component = (tree, name) => nodes(tree, node => node.type.name === name)[0];
const fixture = () => environment({ request: async path => path.includes("/history") ? history() : { session } });

test("ordinary Session preview starts at navigation width and preserves intentional resizing and draft", async context => {
  const previousWindow = globalThis.window; const previousStorage = globalThis.sessionStorage;
  globalThis.window = { innerWidth: 2048, addEventListener() {}, removeEventListener() {}, requestAnimationFrame() {} };
  globalThis.sessionStorage = { getItem: () => null };
  context.after(() => { globalThis.window = previousWindow; globalThis.sessionStorage = previousStorage; });
  const env = environment({ location: { search: "?new=1", state: { sidebarTab: "chat" } }, request: async path => {
    const data = path === "/api/models" ? { models: [] } : path.includes("session-projects") ? { projects: [] } : { sessions: [] };
    return { ...data, json: async () => data };
  } });
  const subject = renderer(AppPageContent, { agentId: "agent", workspaceDraft: false, location: env.location, modelsVersion: 0 }, env);
  const panel = () => component(subject.tree, "ConnectedWorkspaceContextPanel");
  try {
    await subject.settle();
    assert.equal(panel().props.browserWidthPx, 270);
    component(subject.tree, "WorkspaceComposer").props.onDraftChange("Synthetic unsent draft"); await subject.settle();
    panel().props.onBrowserWidthChange(286); await subject.settle();
    assert.equal(panel().props.browserWidthPx, 286);
    panel().props.onBrowserWidthChange(12); await subject.settle();
    assert.equal(panel().props.browserWidthPx, 270);
    panel().props.onBrowserWidthChange(10000); await subject.settle();
    assert.equal(panel().props.browserWidthPx, 1298);
    panel().props.onClose(); await subject.settle();
    assert.equal(component(subject.tree, "WorkspaceComposer").props.draft, "Synthetic unsent draft");
    assert.equal(panel().props.browserWidthPx, 1298, "closing retains the user's chosen width");
    window.innerWidth = 1280; await subject.settle();
    assert.equal(panel().props.browserWidthPx, 1298, "viewport changes retain the preference for CSS to clip");
    panel().props.onBrowserWidthChange(10000); await subject.settle();
    assert.equal(panel().props.browserWidthPx, 530, "intentional resize reserves 480px beside the visible navigation");
    component(subject.tree, "ShellSidebar").props.onCollapse(); await subject.settle();
    panel().props.onBrowserWidthChange(10000); await subject.settle();
    assert.equal(panel().props.browserWidthPx, 800, "collapsed navigation gives its width back to the available region");
    assert.equal(component(subject.tree, "WorkspaceComposer").props.draft, "Synthetic unsent draft");
  } finally { subject.unmount(); }
});

test("header has only a breadcrumb and List control; overview opens without a fabricated Session", async () => {
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, fixture());
  try {
    await subject.settle();
    const header = nodes(subject.tree, node => node.type === "header")[0];
    assert.equal(component(header, "AgentMark"), undefined);
    const crumb = component(header, "AgentBreadcrumb");
    assert.equal(nodes(crumb.type(crumb.props), node => node.type === "nav").length, 1);
    assert.equal(nodes(header, node => node.props.to?.includes("/settings/agents")).length, 0);
    assert.equal(button(subject.tree, "agentChat.toggleOverview").props["aria-expanded"], false);
    button(subject.tree, "agentChat.toggleOverview").props.onClick(); await subject.settle();
    assert.equal(button(subject.tree, "agentChat.toggleOverview").props["aria-expanded"], true);
    assert.ok(component(subject.tree, "AgentOverviewPanel"));
    assert.equal(component(subject.tree, "AgentSessionPreviewShell"), undefined);
  } finally { subject.unmount(); }
});

test("reference opens one conversation, return and collapse retain selection without navigation", async () => {
  const env = fixture(); const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  try {
    await subject.settle(); component(subject.tree, "AgentMessageList").props.onPreviewSession("work"); await subject.settle();
    assert.deepEqual(env.navigations, []);
    component(subject.tree, "AgentSessionPreviewShell").props.onReturn(); await subject.settle();
    assert.ok(component(subject.tree, "AgentOverviewPanel"));
    assert.equal(button(subject.tree, "agentChat.toggleOverview").props["aria-expanded"], true);
    component(subject.tree, "AgentMessageList").props.onPreviewSession("work"); await subject.settle();
    component(subject.tree, "AgentSessionPreviewShell").props.onClose(); await subject.settle();
    assert.equal(component(subject.tree, "AgentSessionPreviewShell"), undefined);
    button(subject.tree, "agentChat.toggleOverview").props.onClick(); await subject.settle();
    const preview = component(subject.tree, "AgentSessionPreviewShell");
    assert.equal(preview.props.session.sessionId, "work");
    assert.equal(preview.props.onOpenChat, undefined);
    assert.deepEqual(env.navigations, []);
  } finally { subject.unmount(); }
});

test("Agent switch clears panel selection, and empty history has no persistent empty or refresh controls", async () => {
  const env = fixture(); env.agents.push({ ...env.agents[0], id: "other" });
  env.request = async path => path.includes("/history") ? history(path.includes("/other/") ? "other" : "agent", path.includes("/other/") ? [] : [row]) : { session };
  const props = { agentId: "agent" }; const subject = renderer(AgentChatPageContent, props, env);
  try {
    await subject.settle(); component(subject.tree, "AgentMessageList").props.onPreviewSession("work"); await subject.settle();
    props.agentId = "other"; await subject.settle();
    assert.equal(component(subject.tree, "AgentSessionPreviewShell"), undefined);
    assert.equal(button(subject.tree, "agentChat.toggleOverview").props["aria-expanded"], false);
    assert.equal(button(subject.tree, "agentChat.refresh"), undefined);
    assert.equal(nodes(subject.tree, node => node.props.children === "agentChat.noHistory").length, 0);
    assert.equal(component(subject.tree, "AgentInputComposer").props.agentId, "other");
  } finally { subject.unmount(); }
});

test("late file metadata does not reopen a collapsed panel, and file breadcrumb returns to its Session", async () => {
  const pending = deferred(); const env = fixture();
  env.request = async path => path.includes("/history") ? history() : path.endsWith("/file-session") ? pending.promise : { session };
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  const request = { file: { displayName: "Synthetic file.txt" }, invalidateMetadata() {} };
  try {
    await subject.settle();
    component(subject.tree, "AgentMessageList").props.onPreviewFile(request, "file-session"); await subject.settle();
    button(subject.tree, "agentChat.toggleOverview").props.onClick(); await subject.settle();
    pending.resolve({ session: { ...session, id: "file-session" } }); await subject.settle();
    assert.equal(button(subject.tree, "agentChat.toggleOverview").props["aria-expanded"], false);
    button(subject.tree, "agentChat.toggleOverview").props.onClick(); await subject.settle();
    const preview = component(subject.tree, "AgentSessionPreviewShell");
    assert.equal(preview.props.libraryPreview.title, request.file.displayName);
    preview.props.onReturnToSession(); await subject.settle();
    assert.equal(component(subject.tree, "AgentSessionPreviewShell").props.libraryPreview, undefined);
    assert.equal(component(subject.tree, "AgentSessionPreviewShell").props.session.sessionId, "file-session");
    assert.equal(button(subject.tree, "agentChat.toggleOverview").props["aria-expanded"], true);
  } finally { subject.unmount(); }
});

test("Agent shell reads workspace recent sessions using record identities, clears denial and leaves other pages read-free", async () => {
  const calls = []; let denied = false;
  const env = environment({ request: async (path, options) => { calls.push([path, options?.method || "GET"]); if (denied) throw new ApiError("forbidden", 403); return path.includes("session-projects") ? { projects: [] } : { sessions: [session] }; } });
  const props = { children: null, activeAgent: env.agents[0], showRecentSessions: true, recentRevision: "first" };
  const subject = renderer(ShellPage, props, env);
  try {
    await subject.settle();
    assert.deepEqual(calls, [["/api/workspaces/workspace/sessions?agentId=agent", "GET"], ["/api/workspaces/workspace/session-projects?agentId=agent", "GET"]]);
    assert.equal(component(subject.tree, "ShellSidebar").props.recentNavigation.sessions[0].agentId, "worker");
    denied = true; props.recentRevision = "next"; await subject.settle();
    assert.deepEqual(component(subject.tree, "ShellSidebar").props.recentNavigation.sessions, []);
    assert.equal(component(subject.tree, "ShellSidebar").props.recentNavigation.error, true);
  } finally { subject.unmount(); }
  const ordinary = renderer(ShellPage, { children: null }, env); await ordinary.settle(); ordinary.unmount();
  assert.equal(calls.length, 4);
});

test("bot sidebar uses the shared pinned, projects and recent sections", () => {
  const env = environment(); const recentNavigation = { sessions: [session], loading: false, error: false, onRetry() {} };
  const renderTab = value => { const node = component(value, "ConversationTab"); return renderer(node.type, node.props, env).render(); };
  const rendered = [];
  const sessionProps = { sessions: [session], projects: [{ id: "project", name: "Project" }], groupedSessions: { pinned: [session], recent: [session], projectSessions: { project: [session] } }, renderSessionRow: (value, options) => { rendered.push([value, options]); return null; } };
  const ordinary = renderer(ShellSidebar, { workspace: env.workspace, agents: env.agents, initialTab: "chat", sessionProps, recentNavigation }, env);
  const tree = renderTab(ordinary.render());
  for (const label of ["appRoute.pin", "shellSidebar.projects", "shellSidebar.recent"]) assert.ok(nodes(tree, node => node.props.children === label).length);
  assert.equal(nodes(tree, node => node.props.children === "shellSidebar.noSessions").length, 0);
  assert.deepEqual(rendered, [[session, { icon: true }], [session, { nested: true }], [session, undefined]]);
});

test("New Chat keeps the chat sidebar tab and enters the selected bot's Session draft", () => {
  const navigation = [];
  const env = environment({ navigate: (...args) => navigation.push(args) });
  const subject = renderer(ShellSidebar, { workspace: env.workspace, agents: env.agents, activeAgent: env.agents[0], initialTab: "chat" }, env);
  button(subject.render(), "shellSidebar.newConversation").props.onClick();
  assert.deepEqual(navigation, [["/w/workspace/agents/agent?new=1", { state: { sidebarTab: "chat" } }]]);
});

test("ordinary Session rows retain running, unread and pin indicators and all metadata actions", async context => {
  const previousWindow = globalThis.window; const previousStorage = globalThis.sessionStorage;
  globalThis.window = { addEventListener() {}, removeEventListener() {}, requestAnimationFrame() {} };
  globalThis.sessionStorage = { getItem: () => null };
  context.after(() => { globalThis.window = previousWindow; globalThis.sessionStorage = previousStorage; });
  const item = { ...session, isPinned: true, isUnread: true, origin: "automation", hasActiveAgentRun: true };
  const calls = [];
  const env = environment({ location: { search: "?new=1", state: { sidebarTab: "chat" } }, request: async (path, options = {}) => {
    calls.push([path, options]);
    const data = path === "/api/models" ? { models: [] } : path.includes("session-projects") ? { projects: [] }
      : options.method === "PATCH" ? { session: { ...item, ...JSON.parse(options.body) } } : { sessions: [item] };
    return { ...data, json: async () => data };
  } });
  const subject = renderer(AppPageContent, { agentId: "agent", workspaceDraft: false, location: env.location, modelsVersion: 0 }, env);
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

test("a selected unread Session is marked read before the new list commits and after an existing mutation", async () => {
  const pending = deferred(); const calls = []; const errors = [];
  const selected = { ...session, id: "selected", isUnread: true };
  const env = environment({ request: async (path, options = {}) => {
    const metadata = JSON.parse(options.body); calls.push([path, metadata]);
    if (path.endsWith("/work")) return pending.promise;
    return { session: { ...selected, ...metadata } };
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

test("bot sidebar exposes the same Session metadata controls and waits for delete confirmation", async () => {
  let item = { ...session, isPinned: true, isUnread: true, origin: "automation", hasActiveAgentRun: true };
  const calls = [];
  const env = environment({ request: async (path, options = {}) => {
    calls.push([path, options]);
    if (options.method === "PATCH") { item = { ...item, ...JSON.parse(options.body) }; return { session: item }; }
    if (options.method === "DELETE") return undefined;
    return path.includes("session-projects") ? { projects: [] } : { sessions: [item] };
  } });
  const subject = renderer(ShellPage, { children: null, activeAgent: env.agents[0], showRecentSessions: true }, env);
  const row = () => {
    const sidebar = component(subject.tree, "ShellSidebar");
    const value = sidebar.props.sessionProps.renderSessionRow(sidebar.props.sessionProps.sessions[0], { icon: true });
    return typeof value.type === "function" ? renderer(value.type, value.props, env).render() : value;
  };
  const openMenu = async () => { button(row(), "appRoute.conversationActionsForValue").props.onClick(); await subject.settle(); };
  try {
    await subject.settle();
    assert.equal(nodes(row(), node => node.props.to?.includes("?sessionId="))[0].props.to, "/w/workspace/agents/worker?sessionId=work");
    for (const label of ["appRoute.running", "appRoute.pinned", "appRoute.unread"]) assert.ok(nodes(row(), node => node.props["aria-label"] === label).length);
    assert.equal(nodes(row(), node => node.props.children === "appRoute.auto").length, 0, "Agent work remains an ordinary Session without an automatic badge");
    await openMenu(); button(row(), "appRoute.markAsRead").props.onClick(); await subject.settle();
    assert.deepEqual(JSON.parse(calls.find(([, options]) => options.method === "PATCH")[1].body), { isUnread: false });
    assert.equal(nodes(row(), node => node.props["aria-label"] === "appRoute.unread").length, 0);
    await openMenu(); button(row(), "appRoute.rename").props.onClick(); await subject.settle();
    nodes(row(), node => node.type === "input")[0].props.onBlur({ currentTarget: { value: " Renamed from bot " } }); await subject.settle();
    assert.equal(item.title, "Renamed from bot");
    await openMenu(); button(row(), "appRoute.unpin").props.onClick(); await subject.settle();
    assert.equal(component(subject.tree, "ShellSidebar").props.sessionProps.groupedSessions.pinned.length, 0);
    await openMenu(); button(row(), "appRoute.delete").props.onClick(); await subject.settle();
    assert.equal(calls.some(([, options]) => options.method === "DELETE"), false);
    const dialog = component(subject.tree, "ConfirmDialog"); assert.equal(dialog.props.open, true);
    await dialog.props.onConfirm(); await subject.settle();
    assert.equal(calls.filter(([, options]) => options.method === "DELETE").length, 1);
    assert.equal(component(subject.tree, "ShellSidebar").props.sessionProps.sessions.length, 0);
  } finally { subject.unmount(); }
});

test("switching bot aborts a pending Session mutation and rejects its late metadata", async () => {
  const pending = deferred(); let signal;
  const env = environment({ request: async (path, options = {}) => {
    if (options.method === "PATCH") { signal = options.signal; return pending.promise; }
    return path.includes("session-projects") ? { projects: [] } : { sessions: [{ ...session, title: path.includes("agentId=other") ? "Other bot work" : "First bot work" }] };
  } });
  const props = { children: null, activeAgent: env.agents[0], showRecentSessions: true };
  const subject = renderer(ShellPage, props, env);
  const row = () => {
    const sidebar = component(subject.tree, "ShellSidebar");
    const value = sidebar.props.sessionProps.renderSessionRow(sidebar.props.sessionProps.sessions[0]);
    return renderer(value.type, value.props, env).render();
  };
  try {
    await subject.settle(); button(row(), "appRoute.conversationActionsForValue").props.onClick(); await subject.settle();
    button(row(), "appRoute.markAsUnread").props.onClick(); await subject.settle();
    props.activeAgent = { ...env.agents[0], id: "other" }; await subject.settle();
    assert.equal(signal.aborted, true);
    pending.resolve({ session: { ...session, title: "Late first bot work", isUnread: true } }); await subject.settle();
    assert.equal(component(subject.tree, "ShellSidebar").props.sessionProps.sessions[0].title, "Other bot work");
    assert.equal(nodes(row(), node => node.props["aria-label"] === "appRoute.unread").length, 0);
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
