import { Link } from "react-router";
import type { AgentSummary } from "../router";
import { sessionAgentChatPath } from "./agentChatNavigation";

type Props = Readonly<{
  workspaceId: string;
  agent: AgentSummary;
  session: Readonly<{ id: string; workspaceId: string; agentId: string; status: string; title: string }>;
  sessionId: string;
  label: string;
}>;
export function AgentBreadcrumb({ agent, label }: Readonly<{ agent: AgentSummary; label: string }>) {
  return <nav className="workspaceConversationBreadcrumb" aria-label={label}><strong aria-current="page">{agent.name}</strong></nav>;
}
export function SessionAgentBreadcrumb({ workspaceId, agent, session, sessionId, label }: Props) {
  const target = sessionAgentChatPath(workspaceId, agent, session, sessionId);
  return <nav className="workspaceConversationBreadcrumb" aria-label={label}>
    <Link to={target}>{agent.name}</Link><span>/</span><span>{session.title}</span>
  </nav>;
}
