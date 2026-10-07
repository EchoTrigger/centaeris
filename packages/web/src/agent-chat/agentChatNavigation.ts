function identity(value: string) {
  if (!value || value.trim() !== value) throw new Error("agent_chat_identity_invalid");
  return encodeURIComponent(value);
}

export function agentChatPath(workspaceId: string, agentId: string) {
  return `/w/${identity(workspaceId)}/agents/${identity(agentId)}`;
}

export function sessionChatPath(workspaceId: string, agentId: string, sessionId: string) {
  identity(sessionId);
  return `${agentChatPath(workspaceId, agentId)}?${new URLSearchParams({ sessionId })}`;
}

type SessionAgentIdentity = Readonly<{ id: string; workspaceId: string; status: string }>;
export function sessionAgentChatPath(workspaceId: string, agent: SessionAgentIdentity, session: SessionAgentIdentity & Readonly<{ agentId: string }>, sessionId: string) {
  identity(sessionId);
  if (session.id !== sessionId || session.workspaceId !== workspaceId || agent.workspaceId !== workspaceId
    || session.agentId !== agent.id || session.status !== "active" || agent.status !== "active") throw new Error("session_agent_breadcrumb_invalid");
  return agentChatPath(workspaceId, agent.id);
}

export function workspaceChatKind(agentId: string, search: string): "agent" | "session" {
  if (!agentId) return "session";
  const query = new URLSearchParams(search);
  const sessions = query.getAll("sessionId");
  const fresh = query.getAll("new");
  if (sessions.length > 1 || fresh.length > 1 || (sessions.length && fresh.length)
    || (sessions.length && !sessions[0]) || (fresh.length && fresh[0] !== "1")) {
    throw new Error("agent_chat_route_selector_invalid");
  }
  return sessions.length || fresh.length ? "session" : "agent";
}
