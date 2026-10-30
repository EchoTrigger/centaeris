import { agentAvatarSrc } from "./agentAvatar";
export function AgentMark({ agent, className = "", previewSrc = "" }) {
  if (!agent || !["centaeris", "banana"].includes(agent.avatarKind)) {
    throw new Error("Agent avatarKind is invalid");
  }
  return (
    <span className={`${className} shAgentMark`.trim()} aria-hidden="true">
      <img
        src={previewSrc || agentAvatarSrc(agent)}
        alt=""
      />
    </span>
  );
}
