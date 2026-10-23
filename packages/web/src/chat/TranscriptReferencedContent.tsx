import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { useTranslation } from "../i18n";
import { MarkdownContent } from "./MarkdownContent";
import type { TranscriptContentRef } from "./transcriptContract";
import { TranscriptContentReader } from "./transcriptContentReader";
import { loadTranscriptContentRange } from "./transcriptContentRanges";

export function TranscriptReferencedContent({ sessionId, projectionGeneration, reference, mode, allowVisuals = false, deferUntilVisible = false }: Readonly<{
  sessionId: string;
  projectionGeneration: string;
  reference: TranscriptContentRef;
  mode: "markdown" | "plain";
  deferUntilVisible?: boolean;
  allowVisuals?: boolean;
}>) {
  const anchor = useRef<HTMLSpanElement>(null);
  const [visible, setVisible] = useState(!deferUntilVisible);
  useEffect(() => {
    if (!deferUntilVisible || typeof IntersectionObserver === "undefined") { setVisible(true); return; }
    const observer = new IntersectionObserver(entries => {
      if (entries.some(entry => entry.isIntersecting)) { setVisible(true); observer.disconnect(); }
    });
    if (anchor.current) observer.observe(anchor.current);
    return () => { observer.disconnect(); };
  }, [deferUntilVisible]);
  const [reader, setReader] = useState<TranscriptContentReader | null>(null);
  const { refId, revision, byteLength } = reference;
  useEffect(() => {
    if (!visible) return;
    const next = new TranscriptContentReader(
      (offset, signal) => loadTranscriptContentRange({
        sessionId, projectionGeneration, reference: { refId, revision, byteLength },
      }, offset, signal),
    );
    setReader(next);
    void next.loadAll();
    return () => { next.dispose(); };
  }, [sessionId, projectionGeneration, refId, revision, byteLength, visible]);
  const content = reader ? <Content reader={reader} mode={mode} allowVisuals={allowVisuals} /> : null;
  return deferUntilVisible ? <span ref={anchor} style={{ display: "block", minHeight: "1lh" }}>{content}</span> : content;
}

function Content({ reader, mode, allowVisuals }: Readonly<{ reader: TranscriptContentReader; mode: "markdown" | "plain"; allowVisuals: boolean }>) {
  const { t } = useTranslation();
  const state = useSyncExternalStore(reader.subscribe, reader.getSnapshot, reader.getSnapshot);
  return <>
    {state.content ? (mode === "markdown" ? <MarkdownContent text={state.content} allowVisuals={allowVisuals} isStreaming={state.hasMore && !state.error} /> : state.content) : null}
    {state.unavailable ? <span role="alert">{t("transcriptBlockContent.unableToLoadContent")}</span> : null}
  </>;
}
