import { lazy, Suspense } from "react";
import { codePreviewLanguage } from "../chat/codePreviewFormats.mjs";
import { MarkdownContent } from "../chat/MarkdownContent";
import { useTranslation } from "../i18n";

const CodePreview = lazy(() => import("./CodePreview"));

function baseContentType(contentType) {
  return String(contentType || "").split(";", 1)[0].trim().toLowerCase();
}
function PlainTextPreview({ content, locator }) {
  const lines = content.split(/\r?\n/);
  if (!Number.isInteger(locator?.startLine) || !Number.isInteger(locator?.endLine)) {
    return <pre>{content}</pre>;
  }
  const startIndex = Math.max(0, locator.startLine - 1);
  const endIndex = Math.min(lines.length, locator.endLine);
  if (startIndex >= endIndex) throw new Error("citation text locator is outside the preview content");
  const before = lines.slice(0, startIndex).join("\n");
  const evidence = lines.slice(startIndex, endIndex).join("\n");
  const after = lines.slice(endIndex).join("\n");
  return (
    <pre>
      {before ? `${before}\n` : ""}
      <mark>{evidence}</mark>
      {after ? `\n${after}` : ""}
    </pre>
  );
}

export function TextFilePreview({ content, contentType = "", title, locator, renderMarkdown = false, className = "" }) {
  const { t } = useTranslation();
  const language = codePreviewLanguage(title, contentType);
  if (language) {
    return (
      <Suspense fallback={<div className="filePreviewState" role="status">{t("workspaceContextPanel.loadingReferenceFile")}</div>}>
        <CodePreview
          className={className}
          content={content}
          language={language}
          title={title}
          startLine={locator?.startLine}
          endLine={locator?.endLine}
        />
      </Suspense>
    );
  }
  if (renderMarkdown && baseContentType(contentType) === "text/markdown") {
    return <div className={`documentTextPreview ${className}`.trim()}><MarkdownContent text={content} /></div>;
  }
  return <div className={`documentTextPreview ${className}`.trim()}><PlainTextPreview content={content} locator={locator} /></div>;
}
