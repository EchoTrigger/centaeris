import { defaultKeymap } from "@codemirror/commands";
import { bracketMatching, defaultHighlightStyle, foldGutter, syntaxHighlighting } from "@codemirror/language";
import { highlightSelectionMatches, searchKeymap } from "@codemirror/search";
import { EditorState, RangeSetBuilder } from "@codemirror/state";
import {
  Decoration,
  drawSelection,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
} from "@codemirror/view";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { loadCodePreviewLanguage } from "../chat/loadCodePreviewLanguage.mjs";
import { useTranslation } from "../i18n";

function normalizeLine(value) {
  return Number.isInteger(value) && value > 0 ? value : null;
}

function targetLineDecorations(startLine, endLine) {
  const start = normalizeLine(startLine);
  if (!start) return [];
  return EditorView.decorations.compute(["doc"], (state) => {
    const builder = new RangeSetBuilder();
    const safeStart = Math.min(start, state.doc.lines);
    const safeEnd = Math.min(Math.max(normalizeLine(endLine) || safeStart, safeStart), state.doc.lines);
    for (let lineNumber = safeStart; lineNumber <= safeEnd; lineNumber += 1) {
      const line = state.doc.line(lineNumber);
      builder.add(line.from, line.from, Decoration.line({ class: "codePreviewTargetLine" }));
    }
    return builder.finish();
  });
}

export default function CodePreview({ content, language, title, startLine, endLine, className = "" }) {
  const { t } = useTranslation();
  const containerRef = useRef(null);
  const editorRef = useRef(null);
  const languageRequestRef = useRef(0);
  const [languageExtension, setLanguageExtension] = useState([]);
  const [languageError, setLanguageError] = useState(false);

  const loadLanguage = useCallback(async () => {
    const request = languageRequestRef.current + 1;
    languageRequestRef.current = request;
    setLanguageExtension([]);
    setLanguageError(false);
    try {
      const extension = await loadCodePreviewLanguage(language);
      if (languageRequestRef.current === request) setLanguageExtension([extension]);
    } catch {
      if (languageRequestRef.current === request) setLanguageError(true);
    }
  }, [language]);

  useEffect(() => {
    void loadLanguage();
    return () => { languageRequestRef.current += 1; };
  }, [loadLanguage]);

  const extensions = useMemo(() => [
    lineNumbers(),
    foldGutter(),
    highlightSpecialChars(),
    drawSelection(),
    highlightActiveLine(),
    highlightActiveLineGutter(),
    highlightSelectionMatches(),
    bracketMatching(),
    syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
    keymap.of([...searchKeymap, ...defaultKeymap]),
    EditorState.readOnly.of(true),
    EditorView.editable.of(false),
    EditorView.lineWrapping,
    targetLineDecorations(startLine, endLine),
    ...languageExtension,
  ], [endLine, languageExtension, startLine]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return undefined;
    const editor = new EditorView({ parent: container });
    editorRef.current = editor;
    return () => {
      editor.destroy();
      editorRef.current = null;
    };
  }, []);

  useEffect(() => {
    const editor = editorRef.current;
    if (!editor) return;
    editor.setState(EditorState.create({ doc: content, extensions }));
    const target = normalizeLine(startLine);
    if (!target) return;
    const line = editor.state.doc.line(Math.min(target, editor.state.doc.lines));
    editor.dispatch({
      selection: { anchor: line.from },
      effects: EditorView.scrollIntoView(line.from, { y: "center" }),
    });
  }, [content, extensions, startLine]);

  return (
    <section
      className={`codePreview ${className}`.trim()}
      aria-label={title}
      data-language={language}
    >
      <div className="codePreviewEditorMount" ref={containerRef} />
      {languageError ? (
        <div className="codePreviewLanguageError" role="alert">
          <span>{t("codePreview.syntaxHighlightingUnavailable")}</span>
          <button type="button" onClick={() => void loadLanguage()}>{t("codePreview.retry")}</button>
        </div>
      ) : null}
    </section>
  );
}
