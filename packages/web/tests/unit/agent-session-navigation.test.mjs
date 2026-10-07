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
    await router.navigate(navigation.sessionChatPath("workspace", "worker", "work"));
    assert.equal(router.state.historyAction, "PUSH");
    assert.equal(router.state.location.pathname + router.state.location.search, "/w/workspace/agents/worker?sessionId=work");
    const breadcrumb = SessionAgentBreadcrumb({ workspaceId: "workspace", agent, session, sessionId: "work", label: "Current conversation" });
    const link = nodes(breadcrumb, node => typeof node.props.to === "string")[0].props;
    assert.equal(link.to, "/w/workspace/agents/worker"); assert.equal(link.replace, undefined);
    // Browser history is exercised only by this Node memory-router test.
    await router.navigate(-1); assert.equal(router.state.location.pathname, source);
    await router.navigate(1); assert.equal(router.state.location.search, "?sessionId=work");
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
