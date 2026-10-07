export function AgentMark({ agent, className = "" }) {
  if (!agent || !["centaeris", "banana"].includes(agent.avatarKind)) {
    throw new Error("Agent avatarKind is invalid");
  }
  return (
    <span className={`${className} shAgentMark`.trim()} aria-hidden="true">
      <img
        src={agent.avatarKind === "banana" ? "/agent-avatar-banana.png" : "/centaeris-mark.png"}
        alt=""
      />
    </span>
  );
}
