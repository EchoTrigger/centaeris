import type { AssistantExecutionTurn } from "./types";

// Renderer identity only: authoritative text and runtime event identities stay intact.
export const assistantTextIdentity = (
  turn: Pick<AssistantExecutionTurn, "id" | "textSegmentId">,
): string => turn.textSegmentId ?? `${turn.id}-answer-text`;
