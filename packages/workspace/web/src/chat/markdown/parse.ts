import type { Definition, FootnoteDefinition, Nodes, Root } from "mdast";
import { fromMarkdown } from "mdast-util-from-markdown";
import { gfmFromMarkdown } from "mdast-util-gfm";
import { mathFromMarkdown } from "mdast-util-math";
import { gfm } from "micromark-extension-gfm";
import { math } from "micromark-extension-llm-math";

export type MarkdownDocument = Readonly<{
  source: string;
  root: Root;
  definitions: ReadonlyMap<string, Definition>;
  footnotes: ReadonlyMap<string, FootnoteDefinition>;
  footnoteOrder: readonly string[];
}>;

export function parseMarkdown(source: string): MarkdownDocument {
  const root = fromMarkdown(source, {
    extensions: [gfm(), math({ singleDollarTextMath: false })],
    mdastExtensions: [gfmFromMarkdown(), mathFromMarkdown()],
  });
  const definitions = new Map<string, Definition>();
  const footnotes = new Map<string, FootnoteDefinition>();
  const footnoteOrder: string[] = [];
  const visit = (node: Nodes) => {
    if (node.type === "definition" && !definitions.has(node.identifier)) definitions.set(node.identifier, node);
    if (node.type === "footnoteDefinition" && !footnotes.has(node.identifier)) footnotes.set(node.identifier, node);
    if (node.type === "footnoteReference" && !footnoteOrder.includes(node.identifier)) footnoteOrder.push(node.identifier);
    if ("children" in node) for (const child of node.children) visit(child);
  };
  visit(root);
  return { source, root, definitions, footnotes, footnoteOrder };
}

export function markdownSource(document: MarkdownDocument, node: Nodes) {
  return document.source.slice(node.position?.start.offset ?? 0, node.position?.end.offset ?? document.source.length);
}

export function safeMarkdownHref(href: string) {
  const normalized = href.trim().toLowerCase();
  if (normalized.startsWith("/api/artifacts/")) return null;
  return ["https://", "http://", "mailto:", "/", "#"].some(prefix => normalized.startsWith(prefix)) ? href : null;
}
