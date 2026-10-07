import type { TranscriptBlock } from "./transcriptContract.ts";

export type PendingTranscriptMessage = Readonly<{
  text: string;
  startedAtMs: number;
  sessionId: string | null;
  agentRunId: string | null;
}>;

export function hasCommittedPendingMessage(
  pending: PendingTranscriptMessage | null | undefined,
  sessionId: string | null,
  blockIds: readonly string[],
  read: (id: string) => TranscriptBlock | null | undefined,
): boolean {
  if (!pending?.agentRunId || pending.sessionId !== sessionId) return false;
  return blockIds.some((id) => {
    const block = read(id);
    return block?.body.kind === "userText" && block.presentation?.agentRunId === pending.agentRunId;
  });
}

export type TranscriptTurn = {
  id: string;
  userBlockId: string | null;
  processIds: string[];
  answerIds: string[];
};

export function groupTranscriptTurns(
  blockIds: readonly string[],
  read: (id: string) => { body: { kind: string; noticeType?: unknown; content?: unknown } } | undefined | null,
): TranscriptTurn[] {
  const turns: TranscriptTurn[] = [];
  for (const id of blockIds) {
    const body = read(id)?.body;
    const kind = body?.kind;
    const content = body?.content;
    if (kind === "notice" && body?.noticeType === "run_boundary" && content && typeof content === "object"
      && "inlineContent" in content && content.inlineContent === "" && "sourceRef" in content && content.sourceRef === null) continue;
    if (kind === "userText") {
      turns.push({ id, userBlockId: id, processIds: [], answerIds: [] });
      continue;
    }
    if (!turns.length) turns.push({ id: "continued", userBlockId: null, processIds: [], answerIds: [] });
    const turn = turns[turns.length - 1];
    (kind === "assistantText" ? turn.answerIds : turn.processIds).push(id);
  }
  return turns;
}

export function shouldLoadEarlier(scrollTop: number, hasOlder: boolean, loading: boolean) {
  return scrollTop <= 180 && hasOlder && !loading;
}

export function shouldReleaseLoadedHistory(pressure: boolean, followingLatest: boolean, loadingOlder: boolean): boolean {
  return pressure && followingLatest && !loadingOlder;
}
