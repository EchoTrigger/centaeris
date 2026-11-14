import { memo, useEffect, useRef, useState } from "react";
import { useTranslation } from "../../i18n";
import { useResolvedTheme } from "../../theme";
import { HighlightedTranscriptText } from "../TranscriptSearchHighlight";
import { renderMermaid } from "./renderMermaid";

type Request = { code: string; dark: boolean; streaming: boolean };
type Result = Request & { svg: string | null };

export default memo(function MermaidDiagram({ code, streaming }: { code: string; streaming: boolean }) {
  const { t } = useTranslation();
  const dark = useResolvedTheme() === "dark";
  const [result, setResult] = useState<Result | null>(null);
  const latest = useRef<Request | null>(null);
  const running = useRef(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; latest.current = null; };
  }, []);
  useEffect(() => {
    latest.current = { code, dark, streaming };
    if (running.current) return;
    const drawLatest = async () => {
      running.current = true;
      try {
        while (mounted.current && latest.current) {
          const request = latest.current;
          try {
            const svg = await renderMermaid(request.code, request.dark, () => !mounted.current);
            if (mounted.current && latest.current === request) setResult({ ...request, svg });
          } catch {
            if (mounted.current && latest.current === request) setResult(previous => request.streaming && previous?.svg && request.dark === previous.dark && request.code.startsWith(previous.code)
              ? previous : { ...request, svg: null });
          }
          if (latest.current === request) break;
        }
      } finally { running.current = false; }
    };
    void drawLatest();
  }, [code, dark, streaming]);
  const current = result?.dark === dark && (result.code === code || (streaming && code.startsWith(result.code)));
  return current && result.svg
    ? <div className="markdownMermaid" role="img" aria-label={t("markdown.diagram")} dangerouslySetInnerHTML={{ __html: result.svg }} />
    : <>
      <pre className="markdownCode"><code><HighlightedTranscriptText text={code} /></code></pre>
      {!streaming && current && result.svg === null ? <p className="markdownDiagramError" role="status">{t("markdown.mermaidError")}</p> : null}
    </>;
});
