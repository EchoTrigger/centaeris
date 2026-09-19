import type { ChatMessage } from "./types";

export function transcriptMessageGroups(ids: readonly string[], read: (id: string) => ChatMessage | undefined) {
  const groups: string[][] = [];
  for (const id of ids) {
    const message = read(id);
    const previous = groups.at(-1);
    const last = previous ? read(previous[0]) : undefined;
    // Live AgentRun messages already contain their entire process and final.
    if (message?.role === "assistant" && !message.turn.agentRunId
      && previous && last?.role === "assistant" && !last.turn.agentRunId) previous.push(id);
    else groups.push([id]);
  }
  return groups;
}
