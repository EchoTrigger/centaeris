import type { ChatMessage } from "./types";

export function transcriptMessageGroups(ids: readonly string[], read: (id: string) => ChatMessage | undefined) {
  const groups: string[][] = [];
  const boundaries: string[] = [];
  for (const id of ids) {
    const message = read(id);
    if (message?.role === "assistant" && message.turn.projectionRunId && !message.turn.chunks.length && !message.turn.finalAnswer && !message.transcriptText) { boundaries.push(id); continue; }
    const previous = groups.at(-1);
    const last = previous ? read(previous[0]) : undefined;
    // Live AgentRun messages already contain their entire process and final.
    if (message?.role === "assistant" && !message.turn.agentRunId
      && previous && last?.role === "assistant" && !last.turn.agentRunId) previous.push(id);
    else groups.push([id]);
  }
  for (const id of boundaries) {
    const boundary = read(id);
    if (boundary?.role !== "assistant") continue;
    const group = groups.find(group => group.some(id => { const m = read(id); return m?.role === "assistant" && m.turn.projectionRunId === boundary.turn.projectionRunId; }));
    group?.push(id);
  }
  return groups;
}
