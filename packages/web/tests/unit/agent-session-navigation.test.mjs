import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createMemoryRouter } from "react-router";
import * as navigation from "../../src/agent-chat/agentChatNavigation.ts";
import { environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";

const agent = { id: "worker", workspaceId: "workspace", status: "active", name: "Synthetic worker", avatarKind: "centaeris" };
const session = { id: "work", workspaceId: "workspace", agentId: agent.id, status: "active", title: "Synthetic work session" };
test("ordinary Session breadcrumb targets its authoritative Agent on direct entry and refresh without return state", () => {
  assert.equal(navigation.sessionAgentChatPath("workspace", agent, session, "work"), "/w/workspace/agents/worker");
  assert.equal(navigation.workspaceChatKind(agent.id, "?sessionId=work"), "session");
  assert.equal(navigation.workspaceChatKind(agent.id, "?new=1"), "session");
  const escapedAgent = { ...agent, id: "agent /?", workspaceId: "workspace /?" };
  const escapedSession = { ...session, agentId: escapedAgent.id, workspaceId: escapedAgent.workspaceId };
  assert.equal(navigation.sessionAgentChatPath(escapedAgent.workspaceId, escapedAgent, escapedSession, "work"), "/w/workspace%20%2F%3F/agents/agent%20%2F%3F");
});
test("foreign, stale or deleted breadcrumb identities fail rather than following a supplied return path", () => {
  for (const [owner, current] of [[{ ...agent, workspaceId: "foreign" }, session], [{ ...agent, status: "deleted" }, session], [agent, { ...session, workspaceId: "foreign" }], [agent, { ...session, agentId: "foreign" }], [agent, { ...session, status: "deleted" }], [agent, { ...session, id: "stale" }]]) {
    assert.throws(() => navigation.sessionAgentChatPath("workspace", owner, current, "work"), /session_agent_breadcrumb_invalid/);
  }
});

test("Agent previews a Session in place; ordinary Session navigation and browser Back retain their authoritative owner", async () => {
  const load = subjectLoader();
  const { AgentChatPageContent } = await import(await load(fileURLToPath(new URL("../../src/routes/AgentChatRoute.tsx", import.meta.url))));
  const { SessionAgentBreadcrumb } = await import(await load(fileURLToPath(new URL("../../src/agent-chat/SessionAgentBreadcrumb.tsx", import.meta.url))));
  const source = "/w/workspace/agents/agent";
  const router = createMemoryRouter([{ path: "*", element: null }], { initialEntries: [source] });
  const env = environment({ navigate: (path, options) => router.navigate(path, options), request: async path => path.startsWith("/api/agents/") ? {
    schema: "agent.history.v1", agentId: "agent", sessionId: "coordination", hasMore: false, nextCursor: "reply-position", newestCursor: "reply-position",
    items: [{ kind: "message", cursor: "reply-position", message: { id: "reply", agentRunId: "run", turnId: "turn", toolCallId: "call", sourceSequence: 1, createdAtMs: 1, body: "Synthetic committed reply", sessionRefs: ["work"], fileRefs: [] } }],
  } : { session: { ...session, hasActiveAgentRun: false } } });
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, env);
  try {
    await subject.settle(); nodes(subject.tree, node => node.type.name === "AgentMessageList")[0].props.onPreviewSession("work"); await subject.settle();
    assert.equal(router.state.location.pathname, source);
    assert.equal(nodes(subject.tree, node => node.type.name === "AgentSessionPreviewShell")[0].props.onOpenChat, undefined);
    await router.navigate(navigation.sessionChatPath("workspace", "work"));
    assert.equal(router.state.historyAction, "PUSH");
    assert.equal(router.state.location.pathname + router.state.location.search, "/w/workspace/sessions/work");
    const breadcrumb = SessionAgentBreadcrumb({ workspaceId: "workspace", agent, session, sessionId: "work", label: "Current conversation" });
    const link = nodes(breadcrumb, node => typeof node.props.to === "string")[0].props;
    assert.equal(link.to, "/w/workspace/agents/worker"); assert.equal(link.replace, undefined);
    // Browser history is exercised only by this Node memory-router test.
    await router.navigate(-1); assert.equal(router.state.location.pathname, source);
    await router.navigate(1); assert.equal(router.state.location.pathname, "/w/workspace/sessions/work");
    await router.navigate(link.to); assert.equal(router.state.historyAction, "PUSH"); assert.equal(router.state.location.search, "");
    assert.equal(navigation.workspaceChatKind(agent.id, router.state.location.search), "agent");
  } finally { subject.unmount(); router.dispose(); }
});

test("Agent project New Chat retains the exact project identity and the current conversation sidebar", async () => {
  const { ShellSidebar } = await import(await subjectLoader()(fileURLToPath(new URL("../../src/shell/ShellSidebar.jsx", import.meta.url))));
  for (const projectId of ["project-one", "project /?+%="]) {
    const navigations = [];
    const env = environment({ navigate: (path, options) => navigations.push({ path, options }) });
    const project = { id: projectId, name: "Synthetic project" };
    const sidebar = renderer(ShellSidebar, { workspace: env.workspace, agents: env.agents, activeAgent: env.agents[0], initialTab: "chat", sessionProps: {
      projects: [project], groupedSessions: { pinned: [], recent: [], projectSessions: { [projectId]: [] } }, renderSessionRow: () => null, onCreateProject: () => null,
    } }, env);
    sidebar.render();
    const conversation = nodes(sidebar.tree, node => node.type.name === "ConversationTab")[0];
    const pane = renderer(conversation.type, conversation.props, env); pane.render();
    nodes(pane.tree, node => node.type === "button" && node.props["aria-label"] === "shellSidebar.newConversationInValue")[0].props.onClick();
    const target = new URL(navigations[0].path, "https://fixture.invalid");
    assert.equal(target.pathname, "/w/workspace/agents/agent");
    assert.equal(target.searchParams.get("new"), "1");
    assert.equal(target.searchParams.get("projectId"), projectId);
    assert.deepEqual(navigations[0].options.state, { sidebarTab: "chat" });
    assert.equal(navigation.workspaceChatKind("agent", target.search), "session");
    // The ordinary top entry remains an unassigned Session draft.
    nodes(sidebar.tree, node => node.type === "button" && node.props.children?.includes?.("shellSidebar.newConversation"))[0].props.onClick();
    assert.equal(new URL(navigations[1].path, "https://fixture.invalid").searchParams.has("projectId"), false);
  }
});


test("an independent Session breadcrumb keeps its title when its Agent configuration no longer exists", async () => {
  const { SessionAgentBreadcrumb } = await import(await subjectLoader()(fileURLToPath(new URL("../../src/agent-chat/SessionAgentBreadcrumb.tsx", import.meta.url))));
  const tree = SessionAgentBreadcrumb({ workspaceId: "workspace", agent: null, session, sessionId: "work", label: "Current conversation" });
  assert.equal(nodes(tree, node => typeof node.props.to === "string").length, 0);
  assert.equal(nodes(tree, node => node.type === "span")[0].props.children, session.title);
});

async function independentSessionRouter(initialPath) {
  const { createJsxSubjectLoader } = await import("./jsxSubject.mjs");
  const stub = code => `data:text/javascript;base64,${Buffer.from(code).toString("base64")}`;
  const requests = [];
  const sessions = [session, { ...session, id: "other-work", agentId: "retired-worker" }];
  globalThis.__independentSessionRouteFixture = async path => {
    requests.push(path);
    if (path === "/api/me") return { user: { id: "user" } };
    if (path === "/api/workspaces") return { workspaces: [{ id: "workspace", role: "member" }] };
    if (path.endsWith("/default-agent")) return { agent: { ...agent, id: "default-agent" } };
    if (path.endsWith("/agents")) return { agents: [{ ...agent, id: "default-agent" }] };
    const selected = sessions.find(item => path === `/api/sessions/${item.id}`);
    if (selected) return { session: selected };
    throw new Error(`unexpected route request: ${path}`);
  };
  const overrides = new Map([
    ["react-router", stub(`export * from ${JSON.stringify(import.meta.resolve("react-router"))}; export const createBrowserRouter=routes=>routes;`)],
    ["./api", stub("export class ApiError extends Error{}; export const apiJson=(...args)=>globalThis.__independentSessionRouteFixture(...args); export const requireWorkspaces=value=>value; export const hasAuthenticationRequiredHandler=()=>false; export const isAuthenticationRequired=()=>false; export const setAuthenticationRequiredHandler=()=>{};")],
    ["./components/SessionExpiredDialog", stub("export const SessionExpiredDialog=()=>null;")],
    ["./routes/PasswordResetRoute", stub("export const ForgotPasswordRoute=()=>null; export const ResetPasswordRoute=()=>null;")],
  ]);
  for (const name of ["LibraryObjectRoute", "LibraryRoute", "LoginRoute", "InvitationActivationRoute", "SettingsRoute", "WorkspaceChooserRoute", "WorkspaceHomeRoute", "TrashSessionRoute"]) {
    overrides.set(`./routes/${name}`, stub("export default ()=>null;"));
  }
  const { createRouter } = await import(await createJsxSubjectLoader(overrides)(fileURLToPath(new URL("../../src/router.tsx", import.meta.url))));
  const router = createMemoryRouter(createRouter(), { initialEntries: [initialPath] });
  if (!router.state.initialized) await new Promise(resolve => {
    const unsubscribe = router.subscribe(state => { if (state.initialized) { unsubscribe(); resolve(); } });
  });
  return { router, requests, sessions };
}

async function independentSessionLayout(router) {
  const stub = code => `data:text/javascript;base64,${Buffer.from(code).toString("base64")}`;
  const harness = new URL("./componentHarness.mjs", import.meta.url).href;
  const load = subjectLoader({ extraOverrides: [
    ["react-router", stub(`export {matchPath} from ${JSON.stringify(import.meta.resolve("react-router"))}; import {context} from ${JSON.stringify(harness)}; export const Link=()=>null; export const Navigate=()=>null; export const Outlet=()=>null; export const useLocation=()=>context.location; export const useNavigate=()=>context.navigate; export const useRouteLoaderData=id=>context.loaderData[id];`)],
    ["./AppRoute", stub("export const AppPageContent=()=>null;")],
    ["./AgentChatRoute", stub("export const AgentChatPageContent=()=>null;")],
  ] });
  const { default: WorkspaceChatLayout } = await import(await load(fileURLToPath(new URL("../../src/routes/WorkspaceHomeRoute.jsx", import.meta.url))));
  const env = environment({ location: router.state.location, loaderData: router.state.loaderData });
  const layout = renderer(WorkspaceChatLayout, {}, env);
  async function renderCurrentRoute() {
    env.location = router.state.location; env.loaderData = router.state.loaderData;
    await layout.settle();
    assert.equal(nodes(layout.tree, node => node.props.children === "workspaceHomeRoute.noAgentsAreAvailableInThisWorkspaceYet").length, 0, "a readable Session must not display the no-agents empty page");
    return nodes(layout.tree, node => node.type.name === "AppPageContent")[0];
  }
  return { layout, renderCurrentRoute };
}

test("workspace navigation loads each independent Session after its Agent is replaced or retired", async () => {
  const { router, requests, sessions } = await independentSessionRouter("/w/workspace/app");
  const { layout, renderCurrentRoute } = await independentSessionLayout(router);
  try {
    assert.equal(router.state.loaderData.workspace.selectedSession, null);
    assert.equal((await renderCurrentRoute()).props.initialSession, null);
    await router.navigate("/w/workspace/sessions/work");
    assert.deepEqual(router.state.loaderData.workspace.selectedSession, sessions[0], "opening an old Session from workspace home must load its authoritative Session before rendering");
    assert.deepEqual((await renderCurrentRoute()).props.initialSession, sessions[0]);
    await router.navigate("/w/workspace/sessions/other-work");
    assert.deepEqual(router.state.loaderData.workspace.selectedSession, sessions[1], "switching Sessions must refresh the retained workspace loader");
    assert.equal(requests.filter(path => path === "/api/sessions/other-work").length, 1);
    assert.deepEqual((await renderCurrentRoute()).props.initialSession, sessions[1]);
    await router.navigate("/w/workspace/app");
    assert.equal(router.state.loaderData.workspace.selectedSession, null, "workspace home must not retain a prior Session selection");
    assert.equal((await renderCurrentRoute()).props.initialSession, null);
  } finally { layout.unmount(); router.dispose(); delete globalThis.__independentSessionRouteFixture; }
});

test("legacy Agent Session links load the authoritative Session and preserve it through route changes", async () => {
  const { router, sessions } = await independentSessionRouter("/w/workspace/agents/worker?sessionId=work");
  const { layout, renderCurrentRoute } = await independentSessionLayout(router);
  try {
    assert.deepEqual(router.state.loaderData.workspace.selectedSession, sessions[0]);
    assert.deepEqual((await renderCurrentRoute()).props.initialSession, sessions[0]);
    await router.navigate("/w/workspace/sessions/other-work");
    assert.deepEqual(router.state.loaderData.workspace.selectedSession, sessions[1]);
    await router.navigate("/w/workspace/agents/retired-worker?sessionId=other-work");
    assert.deepEqual(router.state.loaderData.workspace.selectedSession, sessions[1]);
    assert.deepEqual((await renderCurrentRoute()).props.initialSession, sessions[1]);
  } finally { layout.unmount(); router.dispose(); delete globalThis.__independentSessionRouteFixture; }
});
