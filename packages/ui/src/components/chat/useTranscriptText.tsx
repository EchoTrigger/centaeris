import { useEffect, useMemo, useState } from "react";
import { t } from "../../i18n";
import { loadTranscriptContentRange } from "./transcriptContentRanges";
import type { ChatMessage } from "./types";

export function useTranscriptText(message: ChatMessage | undefined) {
  const identity = message?.transcriptText;
  const [result, setResult] = useState<{ identity: typeof identity; text: string } | null>(null);
  const [failed, setFailed] = useState(false);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    void attempt;
    setResult(null);
    setFailed(false);
    if (!identity) return;
    let active = true;
    void (async () => {
      let offset = "0";
      const parts: string[] = [];
      while (active) {
        const page = await loadTranscriptContentRange(identity, offset);
        if (!active) return;
        parts.push(page.content);
        if (!page.hasMore) {
          setResult({ identity, text: parts.join("") });
          return;
        }
        if (BigInt(page.endOffset) <= BigInt(offset)) throw new Error("content range made no progress");
        offset = page.endOffset;
      }
    })().catch(() => { if (active) setFailed(true); });
    return () => { active = false; };
  }, [identity, attempt]);
  const resolved = useMemo(() => {
    if (!message || !identity || result?.identity !== identity) return message;
    const text = result.text;
    if (message.role === "user") return { ...message, text };
    const chunks = message.turn.chunks;
    return { ...message, turn: { ...message.turn,
      finalAnswer: chunks.length === 0 ? text : message.turn.finalAnswer,
      chunks: chunks.map((chunk) => chunk.kind === "reasoning" || chunk.kind === "narrative"
        ? { ...chunk, text } : chunk.kind === "task" ? { ...chunk, task: { ...chunk.task, summary: text } } : chunk),
    } };
  }, [message, identity, result]);
  const status = identity && result?.identity !== identity ? (
    failed ? <span role="alert">{t("transcriptText.failed")}
      <button type="button" onClick={() => setAttempt((value) => value + 1)}>{t("transcriptText.retry")}</button>
    </span> : <span role="status">{t("transcriptText.loading")}</span>
  ) : null;
  return { message: resolved, status, loading: Boolean(status) };
}
