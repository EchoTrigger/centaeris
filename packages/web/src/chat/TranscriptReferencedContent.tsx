import { useEffect, useState, useSyncExternalStore } from "react";
import { useTranslation } from "../i18n";
import { MarkdownContent } from "./MarkdownContent";
import type { TranscriptContentRef } from "./transcriptContract";
import { TranscriptContentReader } from "./transcriptContentReader";
import { loadTranscriptContentRange } from "./transcriptContentRanges";

export function TranscriptReferencedContent({ sessionId, projectionGeneration, reference, mode }: Readonly<{
  sessionId: string;
  projectionGeneration: string;
  reference: TranscriptContentRef;
  mode: "markdown" | "plain";
}>) {
  const [reader, setReader] = useState<TranscriptContentReader | null>(null);
  const { refId, revision, byteLength } = reference;
  useEffect(() => {
    const next = new TranscriptContentReader(
      (offset, signal) => loadTranscriptContentRange({
        sessionId, projectionGeneration, reference: { refId, revision, byteLength },
      }, offset, signal),
    );
    setReader(next);
    void next.loadAll();
    return () => { next.dispose(); };
  }, [sessionId, projectionGeneration, refId, revision, byteLength]);
  return reader ? <Content reader={reader} mode={mode} /> : null;
}

function Content({ reader, mode }: Readonly<{ reader: TranscriptContentReader; mode: "markdown" | "plain" }>) {
  const { t } = useTranslation();
  const state = useSyncExternalStore(reader.subscribe, reader.getSnapshot, reader.getSnapshot);
  return <>
    {state.content ? (mode === "markdown" ? <MarkdownContent text={state.content} /> : state.content) : null}
    {state.unavailable ? <span role="alert">{t("transcriptBlockContent.unableToLoadContent")}</span> : null}
  </>;
}
