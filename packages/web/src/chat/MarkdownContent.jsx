import { useStreamPresentation } from "../useStreamPresentation";
import { Fragment, memo, useEffect, useMemo, useRef } from "react";

function safeHref(href) {
  const normalized = href.trim().toLowerCase();
  if (normalized.startsWith("/api/artifacts/")) return null;
  if (
    normalized.startsWith("https://")
    || normalized.startsWith("http://")
    || normalized.startsWith("mailto:")
    || normalized.startsWith("/")
    || normalized.startsWith("#")
  ) return href;
  return null;
}
function renderInline(text, keyPrefix) {
  const nodes = [];
  const pattern = /(\*\*[^*]+\*\*|`[^`]+`|\[[^\]]+\]\([^)]+\))/g;
  let cursor = 0;
  for (const match of text.matchAll(pattern)) {
    const index = match.index || 0;
    if (index > cursor) nodes.push(<Fragment key={`${keyPrefix}:text:${cursor}`}>{text.slice(cursor, index)}</Fragment>);
    const token = match[0];
    if (token.startsWith("**")) {
      nodes.push(<strong key={`${keyPrefix}:strong:${index}`}>{token.slice(2, -2)}</strong>);
    } else if (token.startsWith("`")) {
      nodes.push(<code key={`${keyPrefix}:code:${index}`}>{token.slice(1, -1)}</code>);
    } else {
      const link = token.match(/^\[([^\]]+)\]\(([^)]+)\)$/);
      if (!link) throw new Error("markdown link parser mismatch");
      const href = safeHref(link[2]);
      nodes.push(href
        ? <a key={`${keyPrefix}:link:${index}`} href={href} target={href.startsWith("http") ? "_blank" : undefined} rel="noreferrer">{link[1]}</a>
        : <Fragment key={`${keyPrefix}:link:${index}`}>{link[1]}</Fragment>);
    }
    cursor = index + token.length;
  }
  if (cursor < text.length) nodes.push(<Fragment key={`${keyPrefix}:text:${cursor}`}>{text.slice(cursor)}</Fragment>);
  return nodes;
}

function tableDivider(line) {
  return /^\s*\|?\s*:?-{3,}:?\s*(\|\s*:?-{3,}:?\s*)+\|?\s*$/.test(line);
}

function tableCells(line) {
  return line.trim().replace(/^\|/, "").replace(/\|$/, "").split("|").map((cell) => cell.trim());
}

export function createMarkdownBlockProjection() {
  return {
    sourceText: "",
    sealedBlocks: [],
    sealedEnd: 0,
    scanOffset: 0,
    lineStart: 0,
    inFence: false,
    nextBlockId: 0,
  };
}

export function updateMarkdownBlockProjection(previous, text, finalize = false) {
  if (!text.startsWith(previous.sourceText)) {
    return updateMarkdownBlockProjection(createMarkdownBlockProjection(), text, finalize);
  }
  const appendedBlocks = [];
  let sealedEnd = previous.sealedEnd;
  let lineStart = previous.lineStart;
  let inFence = previous.inFence;
  let nextBlockId = previous.nextBlockId;
  const sealThrough = (end) => {
    const blockText = text.slice(sealedEnd, end);
    if (blockText.trim()) appendedBlocks.push({ id: nextBlockId++, text: blockText });
    sealedEnd = end;
  };
  for (let index = previous.scanOffset; index < text.length; index += 1) {
    if (text[index] !== "\n") continue;
    const trimmedLine = text.slice(lineStart, index).trim();
    if (trimmedLine.startsWith("```")) {
      inFence = !inFence;
      if (!inFence) sealThrough(index + 1);
    } else if (!inFence && !trimmedLine) {
      sealThrough(index + 1);
    }
    lineStart = index + 1;
  }
  if (finalize) {
    sealThrough(text.length);
    lineStart = text.length;
  }
  return {
    sourceText: text,
    sealedBlocks: appendedBlocks.length ? [...previous.sealedBlocks, ...appendedBlocks] : previous.sealedBlocks,
    sealedEnd,
    scanOffset: text.length,
    lineStart,
    inFence,
    nextBlockId,
  };
}

export const MarkdownContent = memo(function MarkdownContent(/** @type {{ text: string }} */ { text }) {
  const lines = text.trim().split(/\r?\n/);
  const nodes = [];
  let index = 0;
  let nodeIndex = 0;
  while (index < lines.length) {
    const line = lines[index];
    const trimmed = line.trim();
    if (!trimmed) {
      index += 1;
      continue;
    }
    if (trimmed.startsWith("```")) {
      const language = trimmed.slice(3).trim();
      const code = [];
      index += 1;
      while (index < lines.length && !lines[index].trim().startsWith("```")) code.push(lines[index++]);
      if (index < lines.length) index += 1;
      nodes.push(<pre className="markdownCode" key={`code:${nodeIndex++}`}><code data-language={language || undefined}>{code.join("\n")}</code></pre>);
      continue;
    }
    const heading = line.match(/^(#{1,3})\s+(.+)$/);
    if (heading) {
      const content = renderInline(heading[2], `heading:${nodeIndex}`);
      nodes.push(heading[1].length === 1
        ? <h1 key={`heading:${nodeIndex++}`}>{content}</h1>
        : heading[1].length === 2
          ? <h2 key={`heading:${nodeIndex++}`}>{content}</h2>
          : <h3 key={`heading:${nodeIndex++}`}>{content}</h3>);
      index += 1;
      continue;
    }
    if (index + 1 < lines.length && line.includes("|") && tableDivider(lines[index + 1])) {
      const headers = tableCells(line);
      const rows = [];
      index += 2;
      while (index < lines.length && lines[index].includes("|") && lines[index].trim()) rows.push(tableCells(lines[index++]));
      nodes.push(
        <div className="markdownTable" key={`table:${nodeIndex++}`}>
          <table>
            <thead><tr>{headers.map((header, cell) => <th key={cell}>{renderInline(header, `th:${nodeIndex}:${cell}`)}</th>)}</tr></thead>
            <tbody>{rows.map((row, rowIndex) => <tr key={rowIndex}>{headers.map((_, cell) => <td key={cell}>{renderInline(row[cell] || "", `td:${nodeIndex}:${rowIndex}:${cell}`)}</td>)}</tr>)}</tbody>
          </table>
        </div>,
      );
      continue;
    }
    const list = trimmed.match(/^([-*]|\d+\.)\s+(.+)$/);
    if (list) {
      const ordered = /\d+\./.test(list[1]);
      const items = [];
      while (index < lines.length) {
        const item = lines[index].trim().match(/^([-*]|\d+\.)\s+(.+)$/);
        if (!item || /\d+\./.test(item[1]) !== ordered) break;
        items.push(item[2]);
        index += 1;
      }
      const children = items.map((item, itemIndex) => <li key={itemIndex}>{renderInline(item, `li:${nodeIndex}:${itemIndex}`)}</li>);
      nodes.push(ordered ? <ol key={`list:${nodeIndex++}`}>{children}</ol> : <ul key={`list:${nodeIndex++}`}>{children}</ul>);
      continue;
    }
    const paragraph = [];
    while (index < lines.length && lines[index].trim()) {
      if (paragraph.length && (/^(```|#{1,3}\s+|[-*]\s+|\d+\.\s+)/.test(lines[index].trim()))) break;
      if (paragraph.length && index + 1 < lines.length && lines[index].includes("|") && tableDivider(lines[index + 1])) break;
      paragraph.push(lines[index++]);
    }
    nodes.push(<p key={`paragraph:${nodeIndex++}`}>{renderInline(paragraph.join("\n"), `paragraph:${nodeIndex}`)}</p>);
  }
  return <div className="markdownContent">{nodes}</div>;
});

export const StreamingMarkdownContent = memo(function StreamingMarkdownContent({ text: sourceText, finalized = false }) {
  const text = useStreamPresentation(sourceText, !finalized);
  const committedProjection = useRef(createMarkdownBlockProjection());
  const projection = useMemo(
    () => updateMarkdownBlockProjection(
      committedProjection.current,
      text,
      finalized,
    ),
    [finalized, text],
  );
  useEffect(() => {
    committedProjection.current = projection;
  }, [projection]);
  const activeText = projection.sourceText.slice(projection.sealedEnd);
  return (
    <div className="streamingMarkdownContent">
      {projection.sealedBlocks.map((block) => <MarkdownContent key={block.id} text={block.text} />)}
      {activeText ? <MarkdownContent key={projection.nextBlockId} text={activeText} /> : null}
    </div>
  );
});
