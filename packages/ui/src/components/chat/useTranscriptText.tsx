import { useEffect, useMemo, useState } from "react";
import { t } from "../../i18n";
import { loadTranscriptContentRange } from "./transcriptContentRanges";
import type { ChatMessage } from "./types";

export function useTranscriptText(message: ChatMessage | undefined, enabled = true) {
  const identity = message?.transcriptText;
  const [result, setResult] = useState<{ identity: typeof identity; text: string; endOffset: string; hasMore: boolean; offset: string } | null>(null);
  const [failed, setFailed] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const [cursor, setCursor] = useState<{ identity: typeof identity; offsets: string[]; index: number }>({ identity, offsets: ["0"], index: 0 });
  const current = cursor.identity === identity ? cursor : { identity, offsets: ["0"], index: 0 };
  const offset = current.offsets[current.index];
  useEffect(() => {
    void attempt;
    setResult(null);
    setFailed(false);
    if (!identity || !enabled) return;
    let active = true;
    void loadTranscriptContentRange(identity, offset).then((page) => {
      if (!active) return;
      if (page.hasMore && BigInt(page.endOffset) <= BigInt(offset)) throw new Error("content range made no progress");
      setResult({ identity, text: page.content, endOffset: page.endOffset, hasMore: page.hasMore, offset });
    }).catch(() => { if (active) setFailed(true); });
    return () => { active = false; };
  }, [identity, attempt, enabled, offset]);
  const resolved = useMemo(() => {
    if (!message || !identity || result?.identity !== identity || result.offset !== offset) return message;
    const text = result.text;
    if (message.role === "user") return { ...message, text };
    const chunks = message.turn.chunks;
    return { ...message, turn: { ...message.turn,
      finalAnswer: chunks.length === 0 ? text : message.turn.finalAnswer,
      chunks: chunks.map((chunk) => chunk.kind === "reasoning" || chunk.kind === "narrative"
        ? { ...chunk, text } : chunk.kind === "task" ? { ...chunk, task: { ...chunk.task, summary: text } } : chunk),
    } };
  }, [message, identity, result, offset]);
  const loading = Boolean(enabled && identity && (result?.identity !== identity || result.offset !== offset));
  const status = !enabled || !identity ? null : loading ? (
    failed ? <span role="alert">{t("transcriptText.failed")}
      <button type="button" onClick={() => setAttempt((value) => value + 1)}>{t("transcriptText.retry")}</button>
    </span> : <span role="status">{t("transcriptText.loading")}</span>
  ) : <span className="transcriptPageControls">
    {current.index > 0 ? <button type="button" aria-label="Previous content page" onClick={() => setCursor({ ...current, index: current.index - 1 })}>{t("transcriptText.previousOutput")}</button> : null}
    {result?.hasMore ? <button type="button" aria-label="Next content page" onClick={() => setCursor({ identity, offsets: [...current.offsets.slice(0, current.index + 1), result.endOffset], index: current.index + 1 })}>{t("transcriptText.nextOutput")}</button> : null}
  </span>;
  return { message: resolved, status, loading, paged: current.index > 0 || Boolean(result?.hasMore) };
}
