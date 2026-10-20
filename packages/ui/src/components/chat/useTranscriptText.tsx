import { useEffect, useMemo, useState } from "react";
import visualCatalog from "@centaeris/visualization-react/catalog";
import { t } from "../../i18n";
import { loadTranscriptContentRange } from "./transcriptContentRanges";
import type { ChatMessage } from "./types";

export function useTranscriptText(message: ChatMessage | undefined, enabled = true) {
  const identity = message?.transcriptText;
  const assembleAnswer = message?.role === "assistant" && message.turn.chunks.length === 0;
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

    // Answer Markdown is one document: a visual or ordinary fence may cross any
    // transport page boundary. Tool/reasoning/user output retains on-demand paging.
    const read = async () => {
      const maxAnswerBytes = BigInt(visualCatalog.limits.answerBytes);
      if (assembleAnswer && BigInt(identity.reference.byteLength) > maxAnswerBytes) {
        throw new Error("Referenced answer exceeds the display byte limit");
      }
      let nextOffset = offset;
      const parts: string[] = [];
      let accumulatedBytes = 0;
      for (;;) {
        const page = await loadTranscriptContentRange(identity, nextOffset);
        if (!active) return;
        if (page.hasMore && BigInt(page.endOffset) <= BigInt(nextOffset)) throw new Error("content range made no progress");
        parts.push(page.content);
        accumulatedBytes += new TextEncoder().encode(page.content).byteLength;
        if (assembleAnswer && accumulatedBytes > Number(maxAnswerBytes)) {
          throw new Error("Referenced answer exceeds the display byte limit");
        }
        if (!assembleAnswer || !page.hasMore) {
          setResult({ identity, text: parts.join(""), endOffset: page.endOffset, hasMore: page.hasMore, offset });
          return;
        }
        nextOffset = page.endOffset;
      }
    };
    void read().catch(() => { if (active) setFailed(true); });
    return () => { active = false; };
  }, [identity, attempt, enabled, offset, assembleAnswer]);
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
