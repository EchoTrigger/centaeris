import type { AgentSessionReferenceView } from "./agentChatViewTypes";

export function AgentSessionReference({ session, onPreview }: Readonly<{
  session: AgentSessionReferenceView;
  onPreview: (sessionId: string) => void;
}>) {
  return <button className="agentChatSessionReference" type="button" title={session.title}
    aria-label={session.title} aria-busy={session.runState === "running" || undefined}
    onClick={() => onPreview(session.sessionId)}>
    <span className="agentChatSessionTitle">{session.title}</span>
  </button>;
}
