import { memo, useEffect, useMemo, useRef, useState } from "react";
import type katex from "katex";
import { HighlightedTranscriptText } from "../TranscriptSearchHighlight";

let loading: Promise<typeof katex> | undefined;
function loadKatex() {
  loading ??= Promise.all([import("katex"), import("katex/dist/katex.min.css")]).then(([module]) => module.default);
  return loading;
}

export const MathFormula = memo(function MathFormula({ value, source, display = false, streaming }: { value: string; source: string; display?: boolean; streaming: boolean }) {
  const [renderer, setRenderer] = useState<typeof katex | null>(null);
  const previous = useRef<{ value: string; html: string } | null>(null);
  useEffect(() => {
    let cancelled = false;
    void loadKatex().then(module => { if (!cancelled) setRenderer(module); }, () => {});
    return () => { cancelled = true; };
  }, []);
  const current = useMemo(() => {
    if (!renderer) return null;
    try { return renderer.renderToString(value, { displayMode: display, output: "htmlAndMathml", trust: false, throwOnError: true }); }
    catch { return null; }
  }, [display, renderer, value]);
  useEffect(() => { if (current !== null) previous.current = { value, html: current }; }, [current, value]);
  const html = current ?? (streaming && previous.current && value.startsWith(previous.current.value) ? previous.current.html : null);
  return html === null
    ? <span className={display ? "markdownMathBlock markdownMathSource" : "markdownMathSource"}><HighlightedTranscriptText text={source} /></span>
    : <span className={display ? "markdownMathBlock" : "markdownMathInline"} dangerouslySetInnerHTML={{ __html: html }} />;
});
