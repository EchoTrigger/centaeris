import { Check, Copy } from "lucide-react";
import { lazy, memo, Suspense, useContext, useEffect, useMemo, useState } from "react";
import { useTranslation } from "../../i18n";
import { TranscriptSearchQuery } from "../TranscriptSearchHighlight";
import type { SyntaxRange } from "./highlight";

const MermaidDiagram = lazy(() => import("./MermaidDiagram"));
const noRanges: readonly SyntaxRange[] = [];

function CodeText({ code, ranges }: { code: string; ranges: readonly SyntaxRange[] }) {
  const query = useContext(TranscriptSearchQuery);
  return useMemo(() => {
    const matches: { from: number; to: number }[] = [];
    if (query) {
      const folded = code.toLowerCase(), needle = query.toLowerCase();
      for (let from = folded.indexOf(needle); from >= 0; from = folded.indexOf(needle, from + needle.length)) matches.push({ from, to: from + needle.length });
    }
    const ends = [...new Set([0, code.length, ...ranges.flatMap(range => [range.from, range.to]), ...matches.flatMap(range => [range.from, range.to])])].sort((a, b) => a - b);
    let syntax = 0, match = 0;
    return ends.slice(0, -1).map((from, index) => {
      while (syntax < ranges.length && ranges[syntax].to <= from) syntax++;
      while (match < matches.length && matches[match].to <= from) match++;
      const className = ranges[syntax]?.from <= from ? ranges[syntax].className : undefined;
      const value = code.slice(from, ends[index + 1]);
      return <span key={from} className={className}>{matches[match]?.from <= from ? <mark>{value}</mark> : value}</span>;
    });
  }, [code, query, ranges]);
}

function CopyCode({ code }: { code: string }) {
  const { t } = useTranslation();
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  useEffect(() => {
    if (state !== "copied") return;
    const timer = setTimeout(() => setState("idle"), 1500);
    return () => clearTimeout(timer);
  }, [state]);
  return <>
    <button type="button" className="markdownCodeAction" aria-label={t("markdown.copyCode")} onClick={async () => {
      try {
        await navigator.clipboard.writeText(code);
        setState("copied");
      } catch { setState("failed"); }
    }}>{state === "copied" ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}{t(state === "copied" ? "chat.copied" : "markdown.copyCode")}</button>
    {state === "failed" ? <span className="markdownCodeNotice" role="status">{t("chat.copyFailed")}</span> : null}
  </>;
}

export const CodeBlock = memo(function CodeBlock({ code, language, streaming }: { code: string; language: string; streaming: boolean }) {
  const { t } = useTranslation();
  const isMermaid = language.toLowerCase() === "mermaid";
  const [showSource, setShowSource] = useState(false);
  const [syntax, setSyntax] = useState<{ code: string; ranges: readonly SyntaxRange[] } | null>(null);
  useEffect(() => {
    if (isMermaid && !showSource) return;
    let cancelled = false;
    void import("./highlight").then(module => module.highlightCode(code, language)).then(ranges => {
      if (!cancelled) setSyntax({ code, ranges });
    }, () => { if (!cancelled) setSyntax(null); });
    return () => { cancelled = true; };
  }, [code, isMermaid, language, showSource]);
  const plainCode = <pre className="markdownCode"><code data-language={language}><CodeText code={code} ranges={syntax?.code === code ? syntax.ranges : noRanges} /></code></pre>;
  return <section className="markdownCodeBlock">
    <div className="markdownCodeToolbar">
      {isMermaid ? <div className="markdownCodeModes" aria-label={t("markdown.mermaidView")}>
        <button type="button" className="markdownCodeAction" aria-pressed={!showSource} onClick={() => setShowSource(false)}>{t("markdown.diagram")}</button>
        <button type="button" className="markdownCodeAction" aria-pressed={showSource} onClick={() => setShowSource(true)}>{t("markdown.source")}</button>
      </div> : <span className="markdownCodeLanguage">{language}</span>}
      <CopyCode code={code} />
    </div>
    {isMermaid && !showSource ? <Suspense fallback={plainCode}><MermaidDiagram code={code} streaming={streaming} /></Suspense> : plainCode}
  </section>;
});
