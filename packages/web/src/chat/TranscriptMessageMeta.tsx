import { useEffect, useRef, useState } from "react";
import { Check, Copy } from "lucide-react";
import { useTranslation } from "../i18n";
import type { TranscriptContentRef } from "./transcriptContract";
import { loadTranscriptContentRange } from "./transcriptContentRanges";
import "./transcript-message-meta.css";

export function TranscriptMessageMeta({ text, reference, sessionId, projectionGeneration, observedAtMs, user = false, answer = false }: Readonly<{
  text: string | null;
  reference: TranscriptContentRef | null;
  sessionId: string | null;
  projectionGeneration: string | null;
  observedAtMs?: number;
  user?: boolean;
  answer?: boolean;
}>) {
  const { t, i18n } = useTranslation();
  const [result, setResult] = useState<"idle" | "copying" | "copied" | "failed">("idle");
  const controllerRef = useRef<AbortController | null>(null);
  const resetTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => () => {
    controllerRef.current?.abort();
    if (resetTimerRef.current !== null) clearTimeout(resetTimerRef.current);
  }, []);

  const timestamp = Number.isSafeInteger(observedAtMs) && Number(observedAtMs) >= 0
    && Number(observedAtMs) <= 8_640_000_000_000_000 ? new Date(Number(observedAtMs)) : null;
  const time = timestamp ? new Intl.DateTimeFormat(i18n.language, { hour: "2-digit", minute: "2-digit", hour12: false }).format(timestamp) : "";
  const label = answer ? t("agentRunRow.copyAnswer") : t("agentRunRow.copy");

  async function copy() {
    if (controllerRef.current !== null) return;
    const controller = new AbortController();
    controllerRef.current = controller;
    if (resetTimerRef.current !== null) clearTimeout(resetTimerRef.current);
    setResult("copying");
    try {
      let content = text;
      if (content === null) {
        if (!reference || !sessionId || !projectionGeneration) throw new Error("message content unavailable");
        const parts: string[] = [];
        let offset = "0";
        // Read the authoritative raw source to its declared byte length. The
        // visible reader may still have only a prefix (and a bounded cache).
        while (BigInt(offset) < BigInt(reference.byteLength)) {
          const page = await loadTranscriptContentRange({ sessionId, projectionGeneration, reference }, offset, controller.signal);
          if (BigInt(page.endOffset) <= BigInt(offset)) throw new Error("message content made no progress");
          parts.push(page.content);
          offset = page.endOffset;
        }
        content = parts.join("");
      }
      controller.signal.throwIfAborted();
      await navigator.clipboard.writeText(content);
      if (controller.signal.aborted) return;
      setResult("copied");
      resetTimerRef.current = setTimeout(() => { setResult("idle"); resetTimerRef.current = null; }, 1600);
    } catch {
      if (!controller.signal.aborted) setResult("failed");
    } finally {
      if (controllerRef.current === controller) controllerRef.current = null;
    }
  }

  return <div className={`${user ? "workspaceUserMessageMeta" : "workspaceAssistantMessageMeta"} transcriptMessageMeta${result === "copied" ? " isCopied" : ""}${result === "failed" ? " isFailed" : ""}`}>
    {timestamp ? <time dateTime={timestamp.toISOString()} title={new Intl.DateTimeFormat(i18n.language, { dateStyle: "medium", timeStyle: "short" }).format(timestamp)}>{time}</time> : null}
    <button type="button" onClick={() => void copy()} aria-label={label} title={label} aria-busy={result === "copying"} aria-disabled={result === "copying"}>
      {result === "copied" ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
    </button>
    {result === "copied" ? <span className="transcriptMessageCopyStatus" role="status">{t("transcriptBlockContent.copied")}</span> : null}
    {result === "failed" ? <span className="transcriptMessageCopyError" role="alert">{t("transcriptBlockContent.copyFailed")}</span> : null}
  </div>;
}
