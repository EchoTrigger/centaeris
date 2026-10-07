import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { button, nodes, environment, renderer, subjectLoader } from "./componentHarness.mjs";
const { AgentChatPageContent } = await import(await subjectLoader()(fileURLToPath(new URL("../../src/routes/AgentChatRoute.tsx", import.meta.url))));
const row = { kind: "message", cursor: "reply-position", message: { id: "reply", agentRunId: "own-run", turnId: "turn", toolCallId: "call", sourceSequence: 1, createdAtMs: 1, body: "reply", sessionRefs: ["work"], fileRefs: ["bound-input"] } };
const history = { schema: "agent.history.v1", agentId: "agent", sessionId: "coord", items: [row], nextCursor: "tail", newestCursor: "tail", hasMore: false };
test("Agent overview lists independently admitted Work Sessions without displaying its coordination run", async () => {
  const session = { id: "work", workspaceId: "workspace", agentId: "worker", projectId: null, title: "Work", origin: "automation", status: "active", deletedAt: null, isPinned: false, isUnread: false, hasActiveAgentRun: false, updatedAt: "2026-10-05T00:00:00Z", initialInputOrigin: null };
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, environment({ request: async path => path.includes("/history") ? history : path.includes("/work-sessions") ? { schema: "agent.work_sessions.v1", agentId: "agent", workspaceId: "workspace", sessions: [session], nextAfterSessionId: null, hasMore: false } : { session } }));
  await subject.settle();
  button(subject.tree, "agentChat.toggleOverview").props.onClick(); await subject.settle();
  const overview = nodes(subject.tree, node => node.type.name === "AgentOverviewPanel")[0];
  assert.ok(overview); assert.equal(overview.props.sessionId, undefined);
  assert.deepEqual(overview.props.sessions.map(item => item.sessionId), ["work"]); subject.unmount();
});
test("Session preview directly uses the complete conversation projection", async () => {
  const subject = renderer(AgentChatPageContent, { agentId: "agent" }, environment({ request: async path => path.includes("/history") ? history : { session: { id: "work", workspaceId: "workspace", agentId: "worker", title: "Work", status: "active", hasActiveAgentRun: false } } }));
  await subject.settle();
  nodes(subject.tree, node => node.type.name === "AgentMessageList")[0].props.onPreviewSession("work"); await subject.settle();
  const shell = nodes(subject.tree, node => node.type.name === "AgentSessionPreviewShell")[0];
  assert.equal(shell.props.conversation.type.name, "AgentSessionActivityPreview");
  assert.equal(shell.props.conversation.props.sessionId, "work");
  assert.equal(shell.props.outputs, undefined);
  subject.unmount();
});
