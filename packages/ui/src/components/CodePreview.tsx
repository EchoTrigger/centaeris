import { useEffect, useMemo, useRef } from "react";
import { EditorState, RangeSetBuilder } from "@codemirror/state";
import { EditorView, Decoration, drawSelection, highlightActiveLine, highlightActiveLineGutter, highlightSpecialChars, keymap, lineNumbers } from "@codemirror/view";
import { defaultKeymap } from "@codemirror/commands";
import { syntaxHighlighting, HighlightStyle, defaultHighlightStyle, bracketMatching, foldGutter } from "@codemirror/language";
import { cpp } from "@codemirror/lang-cpp";
import { css } from "@codemirror/lang-css";
import { html } from "@codemirror/lang-html";
import { java } from "@codemirror/lang-java";
import { javascript } from "@codemirror/lang-javascript";
import { json } from "@codemirror/lang-json";
import { python } from "@codemirror/lang-python";
import { rust } from "@codemirror/lang-rust";

type CodePreviewProps = {
  content: string;
  path?: string;
  targetLine?: number;
  targetEndLine?: number;
  variant?: "file" | "diff";
};

const getLanguageExtension = (path = "", variant: "file" | "diff" = "file") => {
  if (variant === "diff") {
    return [];
  }
  const extension = path.split(".").pop()?.toLowerCase() ?? "";
  switch (extension) {
    case "c":
    case "cc":
    case "cpp":
    case "cxx":
    case "h":
    case "hpp":
      return cpp();
    case "css":
      return css();
    case "htm":
    case "html":
      return html();
    case "java":
      return java();
    case "js":
    case "jsx":
      return javascript({ jsx: true });
    case "json":
      return json();
    case "py":
      return python();
    case "rs":
      return rust();
    case "ts":
      return javascript({ typescript: true });
    case "tsx":
      return javascript({ jsx: true, typescript: true });
    default:
      return [];
  }
};

const normalizeTargetLine = (value: number | undefined): number | undefined => {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 1) {
    return undefined;
  }
  return Math.floor(value);
};

const buildTargetLineExtension = (startLine: number | undefined, endLine: number | undefined) => {
  const start = normalizeTargetLine(startLine);
  if (!start) {
    return [];
  }
  return EditorView.decorations.compute(["doc"], (state) => {
    const builder = new RangeSetBuilder<Decoration>();
    const docLineCount = state.doc.lines;
    const safeStart = Math.min(start, docLineCount);
    const safeEnd = Math.min(Math.max(normalizeTargetLine(endLine) ?? safeStart, safeStart), docLineCount);
    for (let lineNumber = safeStart; lineNumber <= safeEnd; lineNumber += 1) {
      const line = state.doc.line(lineNumber);
      builder.add(line.from, line.from, Decoration.line({ class: "summaryCodePreviewTargetLine" }));
    }
    return builder.finish();
  });
};

const diffLineClassName = (text: string): string | undefined => {
  if (text.startsWith("@@")) {
    return "summaryCodePreviewDiffHunk";
  }
  if (text.startsWith("+++") || text.startsWith("---")) {
    return "summaryCodePreviewDiffMeta";
  }
  if (text.startsWith("+")) {
    return "summaryCodePreviewDiffAdded";
  }
  if (text.startsWith("-")) {
    return "summaryCodePreviewDiffRemoved";
  }
  return undefined;
};

const buildDiffLineExtension = (variant: "file" | "diff") => {
  if (variant !== "diff") {
    return [];
  }
  return EditorView.decorations.compute(["doc"], (state) => {
    const builder = new RangeSetBuilder<Decoration>();
    for (let lineNumber = 1; lineNumber <= state.doc.lines; lineNumber += 1) {
      const line = state.doc.line(lineNumber);
      const className = diffLineClassName(line.text);
      if (className) {
        builder.add(line.from, line.from, Decoration.line({ class: className }));
      }
    }
    return builder.finish();
  });
};

export function CodePreview({ content, path, targetLine, targetEndLine, variant = "file" }: CodePreviewProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const viewRef = useRef<EditorView | null>(null);
  const extensions = useMemo(
    () => [
      lineNumbers(),
      foldGutter(),
      highlightSpecialChars(),
      drawSelection(),
      highlightActiveLine(),
      highlightActiveLineGutter(),
      bracketMatching(),
      syntaxHighlighting(HighlightStyle.define(defaultHighlightStyle.specs.map((spec) => ({ ...spec, color: spec.color === "#085" || spec.color === "#164" ? "var(--code-string)" : spec.color === "#30a" || spec.color === "#708" ? "var(--code-keyword)" : spec.color === "#a40" ? "var(--code-number)" : "var(--on-surface)" }))), { fallback: true }),
      keymap.of(defaultKeymap),
      EditorState.readOnly.of(true),
      EditorView.editable.of(false),
      getLanguageExtension(path, variant),
      buildTargetLineExtension(targetLine, targetEndLine),
      buildDiffLineExtension(variant),
      EditorView.theme({
        "&": {
          height: "100%",
          backgroundColor: "var(--surface-color)",
          color: "var(--on-surface)",
          fontSize: "13px",
        },
        ".cm-scroller": {
          fontFamily: "var(--font-mono)",
          lineHeight: "21px",
          overflow: "auto",
        },
        ".cm-content": {
          minHeight: "100%",
          caretColor: "var(--on-surface)",
        },
        ".cm-gutters": {
          backgroundColor: "var(--surface-color)",
          borderRight: "1px solid color-mix(in srgb, var(--outline-variant) 58%, transparent)",
          color: "var(--process-text)",
        },
        ".cm-activeLine": {
          backgroundColor: "var(--surface-variant)",
        },
        ".cm-activeLineGutter": {
          backgroundColor: "var(--surface-variant)",
          color: "var(--process-text)",
        },
      }),
    ],
    [path, targetEndLine, targetLine, variant],
  );

  useEffect(() => {
    const container = containerRef.current;
    if (!container) {
      return undefined;
    }
    const view = new EditorView({
      parent: container,
    });
    viewRef.current = view;
    return () => {
      view.destroy();
      viewRef.current = null;
    };
  }, []);

  useEffect(() => {
    const view = viewRef.current;
    if (!view) {
      return;
    }
    view.setState(EditorState.create({ doc: content, extensions }));
    const startLine = normalizeTargetLine(targetLine);
    if (!startLine) {
      return;
    }
    const safeStart = Math.min(startLine, view.state.doc.lines);
    const line = view.state.doc.line(safeStart);
    view.dispatch({
      selection: { anchor: line.from },
      effects: EditorView.scrollIntoView(line.from, { y: "center" }),
    });
  }, [content, extensions, targetLine]);

  return <div className={`summaryCodePreview ${variant === "diff" ? "is-diff" : ""}`} ref={containerRef} />;
}

export default CodePreview;
