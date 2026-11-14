import { VisualMarkdown } from "@centaeris/visuals";
import { renderTranscriptVisual } from "@centaeris/ui/visualizations";
import { createElement, Fragment, memo, useId, type ReactNode } from "react";
import type { Nodes } from "mdast";
import { useTranslation } from "../../i18n";
import { HighlightedTranscriptText } from "../TranscriptSearchHighlight";
import { CodeBlock } from "./CodeBlock";
import { MathFormula } from "./MathFormula";
import { markdownSource, safeMarkdownHref, type MarkdownDocument } from "./parse";
import { useMarkdownSource } from "./useMarkdownSource";
import { useMarkdownDocument } from "./useMarkdownDocument";
import "./markdown.css";

type Context = { document: MarkdownDocument; streaming: boolean; allowVisuals: boolean; id: string };
const text = (value: string) => <HighlightedTranscriptText text={value} />;
const key = (node: Nodes, index: number) => `${node.type}:${node.position?.start.offset ?? index}`;

function children(nodes: readonly Nodes[], context: Context, topLevel = false): ReactNode {
  return nodes.map((node, index) => <Fragment key={key(node, index)}>{renderNode(node, context, topLevel)}</Fragment>);
}

function link(url: string, title: string | null | undefined, content: ReactNode) {
  const href = safeMarkdownHref(url);
  return href ? <a href={href} title={title || undefined} target={/^https?:\/\//i.test(href) ? "_blank" : undefined} rel="noreferrer">{content}</a> : content;
}

function renderNode(node: Nodes, context: Context, topLevel: boolean): ReactNode {
  const { document, streaming } = context;
  switch (node.type) {
    case "root": return children(node.children, context, true);
    case "text": return text(node.value);
    case "paragraph": return <p>{children(node.children, context)}</p>;
    case "heading": return createElement(`h${node.depth}`, {}, children(node.children, context));
    case "emphasis": return <em>{children(node.children, context)}</em>;
    case "strong": return <strong>{children(node.children, context)}</strong>;
    case "delete": return <s>{children(node.children, context)}</s>;
    case "inlineCode": return <code>{text(node.value)}</code>;
    case "break": return <br />;
    case "thematicBreak": return <hr />;
    case "blockquote": return <blockquote>{children(node.children, context)}</blockquote>;
    case "html": return <span className="markdownRawHtml">{text(node.value)}</span>;
    case "link": return link(node.url, node.title, children(node.children, context));
    case "image": return link(node.url, node.title, text(node.alt || node.url));
    case "linkReference": {
      const definition = document.definitions.get(node.identifier);
      return definition ? link(definition.url, definition.title, children(node.children, context)) : text(markdownSource(document, node));
    }
    case "imageReference": {
      const definition = document.definitions.get(node.identifier);
      return definition ? link(definition.url, definition.title, text(node.alt || definition.url)) : text(markdownSource(document, node));
    }
    case "code": {
      if (topLevel && context.allowVisuals && node.lang === "centaeris_visual") return <VisualMarkdown
        text={markdownSource(document, node)} finalized={!streaming} renderVisual={renderTranscriptVisual}
        renderMarkdown={source => <CodeBlock code={source} language="text" streaming={streaming} />} />;
      return <CodeBlock code={node.value} language={node.lang || "text"} streaming={streaming} />;
    }
    case "inlineMath": return <MathFormula value={node.value} source={markdownSource(document, node)} streaming={streaming} />;
    case "math": return <MathFormula value={node.value} source={markdownSource(document, node)} display streaming={streaming} />;
    case "list": {
      const Tag = node.ordered ? "ol" : "ul";
      return <Tag start={node.ordered ? node.start ?? undefined : undefined}>{node.children.map((item, index) => <li key={key(item, index)}>
        {typeof item.checked === "boolean" ? <input type="checkbox" checked={item.checked} disabled readOnly /> : null}
        {item.children.map((child, childIndex) => <Fragment key={key(child, childIndex)}>{!node.spread && !item.spread && child.type === "paragraph"
          ? children(child.children, context) : renderNode(child, context, false)}</Fragment>)}
      </li>)}</Tag>;
    }
    case "table": return <div className="markdownTable"><table>
      <thead><tr>{node.children[0]?.children.map((cell, index) => <th key={key(cell, index)} style={{ textAlign: node.align?.[index] || undefined }}>{children(cell.children, context)}</th>)}</tr></thead>
      <tbody>{node.children.slice(1).map((row, index) => <tr key={key(row, index)}>{row.children.map((cell, cellIndex) => <td key={key(cell, cellIndex)} style={{ textAlign: node.align?.[cellIndex] || undefined }}>{children(cell.children, context)}</td>)}</tr>)}</tbody>
    </table></div>;
    case "footnoteReference": return <sup><a href={`#${context.id}-footnote-${encodeURIComponent(node.identifier)}`}>{document.footnoteOrder.indexOf(node.identifier) + 1}</a></sup>;
    case "definition": case "footnoteDefinition": return null;
    default: return text(markdownSource(document, node));
  }
}

export const MarkdownRenderer = memo(function MarkdownRenderer({ source, streaming, allowVisuals }: { source: string; streaming: boolean; allowVisuals: boolean }) {
  const visible = useMarkdownSource(source, streaming);
  const { document, pending, error } = useMarkdownDocument(visible, streaming);
  const { t } = useTranslation();
  const id = useId();
  if (!document) return <div className="markdownContent" aria-busy={pending || undefined}>
    <div className="markdownSourcePending"><HighlightedTranscriptText text={visible} /></div>
    {error !== null ? <p role="status">{t("markdown.renderError")}</p> : null}
  </div>;
  const context = { document, streaming, allowVisuals, id };
  return <div className="markdownContent" aria-busy={pending || undefined}>
    {children(document.root.children, context, true)}
    {document.footnoteOrder.length ? <ol className="markdownFootnotes">{document.footnoteOrder.map(identifier => {
      const definition = document.footnotes.get(identifier);
      return definition ? <li key={identifier} id={`${id}-footnote-${encodeURIComponent(identifier)}`}>{children(definition.children, context)}</li> : null;
    })}</ol> : null}
  </div>;
});
