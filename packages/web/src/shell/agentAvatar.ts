import { apiUrl } from "../api";

export type AgentAvatarIdentity = Readonly<{ avatarKind: "centaeris" | "banana"; avatarUrl?: string | null }>;

export function builtinAgentAvatarSrc(kind: AgentAvatarIdentity["avatarKind"]): string {
  if (kind === "centaeris") return "/centaeris-mark.png";
  if (kind === "banana") return "/agent-avatar-banana.png";
  throw new Error("Agent avatarKind is invalid");
}

export function agentAvatarSrc(agent: AgentAvatarIdentity): string {
  const fallback = builtinAgentAvatarSrc(agent.avatarKind);
  return agent.avatarUrl ? apiUrl(agent.avatarUrl) : fallback;
}
